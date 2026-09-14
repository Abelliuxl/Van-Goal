import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
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
  });

  final String id;
  final bool fromUser;
  String text;
  bool streaming;
  final List<ToolCall> tools = [];
}

class SessionSummary {
  const SessionSummary({required this.id, required this.title, this.model});

  final String id;
  final String title;
  final String? model;
}

/// Everything the UI draws, fed by the event queue in `crates/mobile`.
///
/// The widget tree owns no state of its own beyond transient things like scroll
/// position, so this is the single source of truth: events come in, are folded
/// into these fields, and listeners rebuild.
class AppState extends ChangeNotifier {
  AppState._();

  static final AppState instance = AppState._();

  final RustCore _core = RustCore.instance;
  Timer? _ticker;

  LinkState link = LinkState.offline;
  String? linkDetail;

  Backend backend = Backend.openclaw;
  String host = '';
  int port = Backend.openclaw.defaultPort;
  bool useTls = true;
  String credential = '';
  String workspace = '';

  List<SessionSummary> sessions = const [];
  List<Message> messages = const [];
  String? sessionId;

  bool sending = false;
  String? error;

  String? clarifyId;
  String? clarifyQuestion;
  List<String> clarifyChoices = const [];

  int _nextLocalId = 0;

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
    _core.send({'cmd': 'settings'});
    _ticker = Timer.periodic(
      const Duration(milliseconds: 60),
      (_) => _drain(),
    );
  }

  @override
  void dispose() {
    _ticker?.cancel();
    _ticker = null;
    super.dispose();
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

  void refreshSessions() => _core.send({'cmd': 'list_sessions'});

  void openSession(String id) {
    messages = const [];
    sending = false;
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
    // Shown straight away rather than waiting for the backend to echo it: the
    // composer is cleared on the same frame, and a prompt that vanished for a
    // round trip would read as a lost message.
    messages = [
      ...messages,
      Message(id: _localId(), text: body, fromUser: true),
    ];
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
        backend = Backend.fromId(event['backend'] as String?);
        host = (event['host'] as String?) ?? host;
        port = (event['port'] as num?)?.toInt() ?? port;
        useTls = (event['use_tls'] as bool?) ?? useTls;
        credential = (event['credential'] as String?) ?? credential;
        workspace = (event['workspace'] as String?) ?? workspace;
      case 'sessions':
        sessions = _readSessions(event['items']);
      case 'sessions_changed':
        refreshSessions();
      case 'transcript':
        messages = _readTranscript(event['items']);
      case 'session':
        sessionId = event['id'] as String?;
      case 'assistant_start':
        _appendAssistant();
      case 'assistant':
        _applyAssistant(event);
      case 'tool':
        _applyTool(event);
      case 'clarify':
        clarifyId = event['request_id'] as String?;
        clarifyQuestion = event['question'] as String?;
        clarifyChoices = ((event['choices'] as List?) ?? const [])
            .map((choice) => choice.toString())
            .toList(growable: false);
      case 'error':
        error = (event['message'] as String?) ?? 'unknown error';
        sending = false;
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

  void _applyAssistant(Map<String, dynamic> event) {
    final text = (event['text'] as String?) ?? '';
    final done = event['done'] == true;
    if (messages.isEmpty || messages.last.fromUser) {
      _appendAssistant();
    }
    final last = messages.last;
    last.text = text;
    last.streaming = !done;
    if (done) {
      sending = false;
      refreshSessions();
    }
  }

  void _applyTool(Map<String, dynamic> event) {
    if (messages.isEmpty || messages.last.fromUser) {
      _appendAssistant();
    }
    messages.last.tools.add(ToolCall(
      name: (event['name'] as String?) ?? 'tool',
      status: (event['status'] as String?) ?? '',
      detail: (event['detail'] as String?) ?? '',
    ));
  }

  void _appendAssistant() {
    messages = [
      ...messages,
      Message(id: _localId(), text: '', fromUser: false, streaming: true),
    ];
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
      final message = Message(
        id: (item['id'] as String?) ?? _localId(),
        text: (item['content'] as String?) ?? '',
        fromUser: item['role'] == 'user',
        streaming: item['streaming'] == true,
      );
      final tools = item['tools'];
      if (tools is List) {
        for (final tool in tools.whereType<Map<String, dynamic>>()) {
          message.tools.add(ToolCall(
            name: (tool['name'] as String?) ?? 'tool',
            status: (tool['status'] as String?) ?? '',
            detail: (tool['detail'] as String?) ?? '',
          ));
        }
      }
      return message;
    }).toList(growable: false);
  }

  String _localId() => 'local-${_nextLocalId++}';
}

/// True on a platform this app has a Rust library for.
bool get isSupportedPlatform =>
    Platform.isAndroid || Platform.isIOS || Platform.isMacOS;
