import 'dart:async';
import 'dart:io';

import 'package:flutter/widgets.dart';
import 'package:path_provider/path_provider.dart';

import 'rust.dart';

/// Which backends this app can talk to.
///
/// The desktop build also offers Codex CLI, Claude Code and Pi, and a managed
/// local `hermes serve`. Those are local subprocesses; none of those binaries
/// exist on a phone and an app cannot spawn them, so `crates/mobile` refuses
/// them and they are not offered here.
enum Backend {
  hermes('hermes', 'Hermes', 9119, false),
  opencode('opencode', 'OpenCode', 4096, false),
  mimocode('mimocode', 'MiMoCode', 4096, false),
  openclaw('openclaw', 'OpenClaw', 18789, true);

  const Backend(this.id, this.label, this.defaultPort, this.prefersTls);

  final String id;
  final String label;
  final int defaultPort;
  final bool prefersTls;

  static Backend fromId(String? id) => Backend.values.firstWhere(
        (backend) => backend.id == id,
        orElse: () => Backend.openclaw,
      );
}

enum LinkState { offline, connecting, online, failed }

/// App-wide text size. The same preference the desktop client writes, so the
/// scale maps onto the same steps.
enum FontSize {
  small('small', 'Small', 0.9),
  normal('default', 'Default', 1.0),
  large('large', 'Large', 1.15),
  extraLarge('extra-large', 'Extra Large', 1.3);

  const FontSize(this.id, this.label, this.scale);

  final String id;
  final String label;
  final double scale;

  static FontSize fromId(String? id) => FontSize.values.firstWhere(
        (size) => size.id == id,
        orElse: () => FontSize.normal,
      );
}

class ToolCall {
  ToolCall({required this.name, required this.status, required this.detail});

  final String name;
  final String status;
  final String detail;
}

class Message {
  Message({
    required this.id,
    required this.text,
    required this.fromUser,
    this.streaming = false,
    List<ToolCall> tools = const [],
  }) : tools = List.of(tools);

  final String id;
  final bool fromUser;
  bool streaming;
  List<ToolCall> tools;
  String text;

  List<Map<String, dynamic>>? _blocks;
  String? _blocksFor;

  /// The message parsed into the blocks the transcript draws, or null while it
  /// is still arriving.
  ///
  /// Streaming text changes on every poll, so it is drawn as plain text and
  /// parsed once when the turn ends. The parse is a local call into the Rust
  /// core — the same parser the desktop renderer uses — and is remembered for
  /// as long as the text is unchanged, so scrolling back through a session does
  /// not repeat it.
  List<Map<String, dynamic>>? get blocks {
    if (streaming || fromUser) {
      return null;
    }
    if (_blocksFor != text) {
      _blocks = RustCore.instance.markdown(text);
      _blocksFor = text;
    }
    return _blocks;
  }
}

class SessionSummary {
  const SessionSummary({required this.id, required this.title, this.model});

  final String id;
  final String title;
  final String? model;
}

/// Everything the UI draws, fed by the event queue in `crates/mobile`.
///
/// The message list is not assembled here. It arrives as a snapshot from the
/// bridge, which owns the rules about what a turn looks like — see
/// `Conversation` in `crates/core/src/chat.rs`. Dart folds no events into
/// messages, which is what keeps the two frontends from disagreeing about them.
class AppState extends ChangeNotifier with WidgetsBindingObserver {
  AppState._();

  static final AppState instance = AppState._();

  final RustCore _core = RustCore.instance;
  Timer? _ticker;
  DateTime? _backgroundedAt;

  LinkState link = LinkState.offline;
  String? linkDetail;

  Backend backend = Backend.openclaw;
  String host = '';
  int port = Backend.openclaw.defaultPort;
  bool useTls = true;
  String credential = '';
  String workspace = '';

  FontSize fontSize = FontSize.normal;
  bool showToolCalls = true;

  List<SessionSummary> sessions = const [];
  List<Message> messages = const [];
  String? sessionId;

  bool sending = false;
  String? error;

  String? clarifyId;
  String? clarifyQuestion;
  List<String> clarifyChoices = const [];

  /// How long the app has to have been away before coming back is treated as a
  /// new connection rather than a blip: a socket that outlived a locked screen
  /// is not necessarily alive, and nothing about it will error on its own.
  static const _staleAfter = Duration(seconds: 5);

  String get title {
    final open = sessionId;
    if (open == null) {
      return 'New chat';
    }
    for (final session in sessions) {
      if (session.id == open) {
        return session.title;
      }
    }
    return 'Chat';
  }

  bool get canSend => link == LinkState.online && !sending;

  /// Bring the client up: name the directory the Rust side may write to, load
  /// what was saved last time, then start draining the event queue.
  ///
  /// The order matters. `vg_set_data_dir` has to be the first call, because the
  /// directory is chosen once and everything after it reads from that choice.
  Future<void> start() async {
    if (_ticker != null) {
      return;
    }
    try {
      final directory = await getApplicationSupportDirectory();
      _core.setDataDir(directory.path);
    } on Object catch (failure) {
      // Not fatal: the app still works, it just will not remember anything.
      debugPrint('van-goal: no writable app directory ($failure)');
    }
    WidgetsBinding.instance.addObserver(this);
    // The Rust side connects on its own when the saved backend is switched on.
    _core.send({'cmd': 'settings'});
    _ticker = Timer.periodic(
      const Duration(milliseconds: 60),
      (_) => _drain(),
    );
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _ticker?.cancel();
    _ticker = null;
    super.dispose();
  }

  /// Android hands the app back after a screen lock or a switch away from it,
  /// with a connection that is usually gone and never notices. Asking for it
  /// again is the difference between a client that works on a phone and one
  /// that has to be reconnected by hand several times an hour.
  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    switch (state) {
      case AppLifecycleState.paused:
      case AppLifecycleState.detached:
      case AppLifecycleState.hidden:
        _backgroundedAt ??= DateTime.now();
      case AppLifecycleState.resumed:
        // Forcing a reconnect throws away a stream that is probably fine, so it
        // is reserved for a return from a real absence. Regaining focus without
        // one — the keyboard opening, a dialog closing — is the app being told
        // it may run again, not that its connection is stale, and it must not
        // interrupt a reply that is being written.
        final away = _backgroundedAt == null
            ? Duration.zero
            : DateTime.now().difference(_backgroundedAt!);
        _backgroundedAt = null;
        _core.send({
          'cmd': 'ensure_connected',
          'force': away >= _staleAfter,
        });
      case AppLifecycleState.inactive:
        break;
    }
  }

  // --------------------------------------------------------------- commands

  /// Save the connection settings and open the connection.
  void connect() {
    _core.send({
      'cmd': 'configure',
      'backend': backend.id,
      'host': host,
      'port': port,
      'use_tls': useTls,
      'credential': credential,
      'workspace': workspace,
    });
    link = LinkState.connecting;
    linkDetail = null;
    error = null;
    notifyListeners();
    _core.send({'cmd': 'connect'});
  }

  /// Close the connection. It stays closed, including across a restart.
  void disconnect() {
    _core.send({'cmd': 'disconnect'});
    link = LinkState.offline;
    linkDetail = null;
    sending = false;
    notifyListeners();
  }

  /// Ask for the connection again after it failed.
  ///
  /// Goes the long way round — the settings are written before the connection is
  /// opened — because a reconnect the user asked for is also the statement that
  /// this backend is the one to come back to: without it the next launch would
  /// find the switch still off and start disconnected again.
  void retry() {
    connect();
  }

  void setFontSize(FontSize size) {
    fontSize = size;
    notifyListeners();
    _core.send({'cmd': 'set_ui', 'font_size': size.id});
  }

  void setShowToolCalls(bool show) {
    showToolCalls = show;
    notifyListeners();
    _core.send({'cmd': 'set_ui', 'show_tool_calls': show});
  }

  void dismissError() {
    error = null;
    notifyListeners();
  }

  void refreshSessions() => _core.send({'cmd': 'list_sessions'});

  void openSession(String id) {
    messages = const [];
    sending = false;
    error = null;
    _core.send({'cmd': 'open_session', 'id': id});
    notifyListeners();
  }

  void newChat() {
    sessionId = null;
    messages = const [];
    sending = false;
    error = null;
    _core.send({'cmd': 'new_session'});
    notifyListeners();
  }

  void send(String text) {
    final body = text.trim();
    if (body.isEmpty || sending) {
      return;
    }
    // The optimistic echo is not drawn here: the prompt is recorded by the
    // bridge, which owns the message list, and comes back in the snapshot that
    // follows. Showing it twice is the bug that arrangement avoids.
    sending = true;
    error = null;
    notifyListeners();
    _core.send({'cmd': 'send', 'text': body});
  }

  void interrupt() {
    _core.send({'cmd': 'interrupt'});
    sending = false;
    notifyListeners();
  }

  void answerClarify(String answer) {
    final request = clarifyId;
    clarifyId = null;
    clarifyQuestion = null;
    clarifyChoices = const [];
    if (request != null) {
      _core.send({'cmd': 'answer', 'request_id': request, 'answer': answer});
    }
    notifyListeners();
  }

  // ---------------------------------------------------------------- events

  void _drain() {
    final events = _core.poll();
    if (events.isEmpty) {
      return;
    }
    for (final event in events) {
      _apply(event);
    }
    notifyListeners();
  }

  void _apply(Map<String, dynamic> event) {
    switch (event['event']) {
      case 'connection':
        _applyConnection(event);
      case 'settings':
        _applySettings(event);
      case 'sessions':
        sessions = _readSessions(event['items']);
      case 'sessions_changed':
        refreshSessions();
      case 'transcript':
        messages = _readTranscript(event['items']);
        sending = event['sending'] == true;
      case 'session':
        sessionId = event['id'] as String?;
      case 'assistant':
        _applyAssistant(event);
      case 'clarify':
        clarifyId = event['request_id'] as String?;
        clarifyQuestion = event['question'] as String?;
        clarifyChoices = ((event['choices'] as List?) ?? const [])
            .map((choice) => choice.toString())
            .toList(growable: false);
      case 'error':
        error = (event['message'] as String?) ?? 'unknown error';
        sending = false;
        // A connection that never came up must not sit on "connecting" forever.
        if (link == LinkState.connecting) {
          link = LinkState.failed;
          linkDetail = error;
        }
    }
  }

  void _applyConnection(Map<String, dynamic> event) {
    switch (event['state']) {
      case 'connecting':
        link = LinkState.connecting;
      case 'connected':
        link = LinkState.online;
        linkDetail = null;
        // A fresh connection has no idea what exists on the other end.
        refreshSessions();
      case 'disconnected':
        link = LinkState.offline;
    }
  }

  void _applySettings(Map<String, dynamic> event) {
    backend = Backend.fromId(event['backend'] as String?);
    host = (event['host'] as String?) ?? host;
    port = (event['port'] as num?)?.toInt() ?? port;
    useTls = (event['use_tls'] as bool?) ?? useTls;
    credential = (event['credential'] as String?) ?? credential;
    workspace = (event['workspace'] as String?) ?? workspace;
    fontSize = FontSize.fromId(event['font_size'] as String?);
    showToolCalls = (event['show_tool_calls'] as bool?) ?? showToolCalls;
  }

  /// Text that streamed further within the message that is already on screen.
  /// Anything else — a new message, a tool call, the end of the turn — arrives
  /// as a new transcript instead.
  void _applyAssistant(Map<String, dynamic> event) {
    final id = event['id'] as String?;
    final text = (event['text'] as String?) ?? '';
    if (id == null) {
      return;
    }
    for (final message in messages) {
      if (message.id == id) {
        message.text = text;
        if (event['done'] == true) {
          message.streaming = false;
          sending = false;
        }
        return;
      }
    }
  }

  List<SessionSummary> _readSessions(Object? raw) {
    if (raw is! List) {
      return const [];
    }
    return raw.whereType<Map<String, dynamic>>().map((item) {
      return SessionSummary(
        id: (item['id'] as String?) ?? '',
        title: (item['title'] as String?) ?? '(untitled)',
        model: item['model'] as String?,
      );
    }).toList(growable: false);
  }

  List<Message> _readTranscript(Object? raw) {
    if (raw is! List) {
      return const [];
    }
    return raw.whereType<Map<String, dynamic>>().map((item) {
      return Message(
        id: (item['id'] as String?) ?? '',
        text: (item['content'] as String?) ?? '',
        fromUser: item['role'] == 'user',
        streaming: item['streaming'] == true,
        tools: [
          for (final tool in ((item['tools'] as List?) ?? const [])
              .whereType<Map<String, dynamic>>())
            ToolCall(
              name: (tool['name'] as String?) ?? 'tool',
              status: (tool['status'] as String?) ?? '',
              detail: (tool['detail'] as String?) ?? '',
            ),
        ],
      );
    }).toList(growable: false);
  }
}

/// True on a platform this app has a Rust library for.
bool get isSupportedPlatform =>
    Platform.isAndroid || Platform.isIOS || Platform.isMacOS;
