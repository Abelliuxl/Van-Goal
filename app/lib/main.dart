import 'package:flutter/material.dart';

import 'markdown.dart';
import 'state.dart';
import 'tools.dart';
import 'waiting.dart';

void main() {
  runApp(const VanGoalApp());
}

/// The palette the desktop client uses, so the two look like one product.
const _surface = Color(0xFF141416);
const _panel = Color(0xFF1A1A1E);
const _raised = Color(0xFF202024);
const _line = Color(0xFF2C2C33);
const _ink = Color(0xFFE8E8EC);
const _inkSoft = Color(0xFF9B9BA6);
const _inkFaint = Color(0xFF6D6D78);
const _accent = Color(0xFF4F8CFF);

const _body = TextStyle(fontSize: 15, height: 1.45, color: _ink);
const _mono = 'monospace';

class VanGoalApp extends StatefulWidget {
  const VanGoalApp({super.key});

  @override
  State<VanGoalApp> createState() => _VanGoalAppState();
}

class _VanGoalAppState extends State<VanGoalApp> {
  final _app = AppState.instance;

  @override
  void initState() {
    super.initState();
    _app.addListener(_onChanged);
    // Naming the data directory has to happen before anything reads it, so the
    // client is started here rather than lazily from the first screen that
    // needs it.
    _app.start();
  }

  @override
  void dispose() {
    _app.removeListener(_onChanged);
    super.dispose();
  }

  void _onChanged() {
    if (mounted) {
      setState(() {});
    }
  }

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Van-Goal',
      debugShowCheckedModeBanner: false,
      theme: ThemeData(
        useMaterial3: true,
        brightness: Brightness.dark,
        colorScheme: ColorScheme.fromSeed(
          seedColor: _accent,
          brightness: Brightness.dark,
        ).copyWith(surface: _surface),
        scaffoldBackgroundColor: _surface,
      ),
      // One text-size preference scales every font in the app at once, the way
      // the desktop client does it. Sizes below are therefore design sizes: none
      // of them is scaled by hand.
      builder: (context, child) => MediaQuery(
        data: MediaQuery.of(context).copyWith(
          textScaler: TextScaler.linear(_app.fontSize.scale),
        ),
        child: child ?? const SizedBox.shrink(),
      ),
      home: const ChatPage(),
    );
  }
}

class ChatPage extends StatefulWidget {
  const ChatPage({super.key});

  @override
  State<ChatPage> createState() => _ChatPageState();
}

class _ChatPageState extends State<ChatPage> {
  final _app = AppState.instance;
  final _composer = TextEditingController();
  final _scroll = ScrollController();

  @override
  void initState() {
    super.initState();
    _app.addListener(_onChanged);
    _app.start();
  }

  @override
  void dispose() {
    _app.removeListener(_onChanged);
    _composer.dispose();
    _scroll.dispose();
    super.dispose();
  }

  /// Follow the newest message. A chat log reads from the bottom, so that is
  /// where new content should appear — unless the reader has scrolled up, in
  /// which case they stay where they are.
  void _onChanged() {
    if (!mounted) {
      return;
    }
    setState(() {});
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!_scroll.hasClients) {
        return;
      }
      final position = _scroll.position;
      final atBottom = position.pixels >= position.maxScrollExtent - 80;
      if (atBottom || _app.sending) {
        _scroll.jumpTo(position.maxScrollExtent);
      }
    });
  }

  void _send() {
    final text = _composer.text;
    if (text.trim().isEmpty) {
      return;
    }
    _composer.clear();
    _app.send(text);
  }

  @override
  Widget build(BuildContext context) {
    final offline = _app.link != LinkState.online;
    return Scaffold(
      appBar: AppBar(
        backgroundColor: _panel,
        surfaceTintColor: Colors.transparent,
        title: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          mainAxisSize: MainAxisSize.min,
          children: [
            Text(
              _app.title,
              maxLines: 1,
              overflow: TextOverflow.ellipsis,
              style: const TextStyle(fontSize: 16),
            ),
            // The state is the part that has to survive a narrow header: at the
            // larger text sizes a long backend name pushed it off the end and
            // left "connec…", which reads as either "connecting" or
            // "connected". The name is allowed to shorten instead.
            Row(
              mainAxisSize: MainAxisSize.min,
              children: [
                Text(
                  _linkState(_app),
                  maxLines: 1,
                  style: TextStyle(
                    fontSize: 11,
                    color: _app.link == LinkState.online ? _accent : _inkFaint,
                  ),
                ),
                Flexible(
                  child: Text(
                    ' · ${_app.backend.label}',
                    maxLines: 1,
                    overflow: TextOverflow.ellipsis,
                    style: const TextStyle(fontSize: 11, color: _inkFaint),
                  ),
                ),
              ],
            ),
          ],
        ),
        actions: [
          // Reconnecting is otherwise a trip through Settings, which is a long
          // way to go for something the app does to itself several times a day.
          if (offline)
            IconButton(
              tooltip: 'Reconnect',
              onPressed: _app.retry,
              icon: Icon(Icons.sync, color: _app.link == LinkState.connecting
                  ? _inkFaint
                  : _accent),
            ),
          IconButton(
            tooltip: 'New chat',
            onPressed: _app.newChat,
            icon: const Icon(Icons.add_comment_outlined),
          ),
          // Refreshes the session list and the open conversation from the
          // backend. Opening the session list is the drawer's job — the system
          // menu button on the left already does that, and a second button for
          // it on this side was one more icon doing nothing.
          IconButton(
            tooltip: 'Refresh',
            onPressed: _app.refreshAll,
            icon: _app.refreshing
                ? const SizedBox(
                    width: 18,
                    height: 18,
                    child: CircularProgressIndicator(strokeWidth: 2),
                  )
                : const Icon(Icons.refresh),
          ),
          IconButton(
            tooltip: 'Settings',
            onPressed: () => _openSettings(context),
            icon: const Icon(Icons.settings_outlined),
          ),
        ],
      ),
      drawer: _SessionDrawer(app: _app),
      body: Column(
        children: [
          if (_app.error != null)
            _ErrorBanner(message: _app.error!, onDismiss: _app.dismissError),
          Expanded(child: _Transcript(app: _app, controller: _scroll)),
          if (_app.clarifyQuestion != null) _ClarifyCard(app: _app),
          _Composer(controller: _composer, app: _app, onSend: _send),
        ],
      ),
    );
  }

  static String _linkState(AppState app) {
    switch (app.link) {
      case LinkState.online:
        return 'connected';
      case LinkState.connecting:
        // A retry loop after a lost connection is a different thing from a
        // first connect, and the wording is the only place the user can see
        // that the app is recovering rather than starting up.
        return app.reconnecting ? 'reconnecting…' : 'connecting…';
      case LinkState.failed:
        return app.linkDetail ?? 'failed';
      case LinkState.offline:
        return 'offline';
    }
  }
}

class _Transcript extends StatelessWidget {
  const _Transcript({required this.app, required this.controller});

  final AppState app;
  final ScrollController controller;

  @override
  Widget build(BuildContext context) {
    if (app.messages.isEmpty && !app.sending) {
      return _EmptyState(app: app);
    }
    // The reply gets a bubble of its own before the gateway has said anything,
    // so a wait always has something moving on screen — see `withWaitingBubble`.
    final messages = withWaitingBubble(app.messages, app.sending);
    return ListView.builder(
      controller: controller,
      padding: const EdgeInsets.fromLTRB(12, 12, 12, 4),
      itemCount: messages.length,
      itemBuilder: (context, index) => _Bubble(
        key: ValueKey(messages[index].id),
        message: messages[index],
        showToolCalls: app.showToolCalls,
      ),
    );
  }
}

class _EmptyState extends StatelessWidget {
  const _EmptyState({required this.app});

  final AppState app;

  @override
  Widget build(BuildContext context) {
    final message = switch (app.link) {
      LinkState.online => 'Send a message, or open a session from the list.',
      LinkState.connecting => 'Connecting to ${app.backend.label}…',
      LinkState.offline =>
        'Not connected. Open Settings to enter a host and a token.',
      LinkState.failed =>
        'Could not connect. Check the address and the token in Settings.',
    };
    return Center(
      child: Padding(
        padding: const EdgeInsets.all(32),
        child: Column(
          mainAxisSize: MainAxisSize.min,
          children: [
            const Text(
              'Van-Goal',
              style: TextStyle(fontSize: 22, fontWeight: FontWeight.w600),
            ),
            const SizedBox(height: 10),
            Text(
              message,
              textAlign: TextAlign.center,
              style: const TextStyle(color: _inkSoft, height: 1.5),
            ),
            if (app.link == LinkState.failed ||
                app.link == LinkState.offline) ...[
              const SizedBox(height: 18),
              FilledButton.tonalIcon(
                onPressed: app.retry,
                icon: const Icon(Icons.sync, size: 18),
                label: const Text('Reconnect'),
              ),
            ],
          ],
        ),
      ),
    );
  }
}

class _Bubble extends StatelessWidget {
  const _Bubble({
    super.key,
    required this.message,
    required this.showToolCalls,
  });

  final Message message;
  final bool showToolCalls;

  @override
  Widget build(BuildContext context) {
    final isUser = message.fromUser;
    final tools = message.tools;
    // A turn that has only run tools so far is working, not empty: as long as
    // the message is streaming and has no text, the dots show there is more
    // coming, whether or not a tool call has already been recorded.
    final waiting = message.streaming && message.text.isEmpty;
    return Align(
      alignment: isUser ? Alignment.centerRight : Alignment.centerLeft,
      child: Container(
        constraints: BoxConstraints(
          maxWidth: MediaQuery.of(context).size.width * 0.86,
        ),
        margin: const EdgeInsets.only(bottom: 12),
        padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
        decoration: BoxDecoration(
          color: isUser ? const Color(0xFF2C3B58) : _raised,
          borderRadius: BorderRadius.circular(14),
          border: Border.all(color: _line),
        ),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          mainAxisSize: MainAxisSize.min,
          children: [
            if (showToolCalls && tools.isNotEmpty) ...[
              ToolCallsView(tools: tools),
              if (!waiting || message.text.isNotEmpty) const SizedBox(height: 8),
            ],
            if (waiting)
              const WaitingDots()
            else if (message.text.isNotEmpty)
              _MarkdownView(message: message),
          ],
        ),
      ),
    );
  }
}

/// A message drawn from the blocks the Rust core parsed it into.
///
/// The core owns the parser — the desktop renderer draws the same blocks — so
/// the two clients cannot disagree about what a message says. While a reply is
/// still streaming it is drawn as plain text instead: re-parsing a table on
/// every token would make the layout jump around as it arrives.
class _MarkdownView extends StatelessWidget {
  const _MarkdownView({required this.message});

  final Message message;

  @override
  Widget build(BuildContext context) {
    final blocks = message.blocks;
    if (blocks == null || blocks.isEmpty) {
      return SelectableText(message.text, style: _body);
    }
    return Column(
      crossAxisAlignment: CrossAxisAlignment.start,
      mainAxisSize: MainAxisSize.min,
      children: [
        for (final block in blocks) _block(block),
      ],
    );
  }

  Widget _block(Map<String, dynamic> block) {
    switch (block['kind']) {
      case 'heading':
        final level = (block['level'] as num?)?.toInt() ?? 1;
        final size = switch (level) {
          1 => 19.0,
          2 => 17.0,
          3 => 16.0,
          _ => 15.0,
        };
        return Padding(
          padding: const EdgeInsets.only(top: 10, bottom: 4),
          child: SelectableText.rich(
            _span(block['runs'], _body.copyWith(
              fontSize: size,
              fontWeight: FontWeight.w600,
              height: 1.3,
            )),
          ),
        );
      case 'bullet':
        return _row('•', block['runs']);
      case 'numbered':
        return _row('${block['number'] ?? ''}.', block['runs']);
      case 'quote':
        return Container(
          width: double.infinity,
          margin: const EdgeInsets.symmetric(vertical: 3),
          padding: const EdgeInsets.only(left: 10),
          decoration: const BoxDecoration(
            border: Border(left: BorderSide(color: _line, width: 3)),
          ),
          child: SelectableText.rich(
            _span(block['runs'], _body.copyWith(color: _inkSoft)),
          ),
        );
      case 'code':
        return Container(
          width: double.infinity,
          margin: const EdgeInsets.symmetric(vertical: 6),
          padding: const EdgeInsets.all(10),
          decoration: BoxDecoration(
            color: _surface,
            borderRadius: BorderRadius.circular(8),
            border: Border.all(color: _line),
          ),
          child: SingleChildScrollView(
            scrollDirection: Axis.horizontal,
            child: SelectableText(
              (block['text'] as String?) ?? '',
              style: const TextStyle(
                fontSize: 13,
                height: 1.35,
                color: _ink,
                fontFamily: _mono,
              ),
            ),
          ),
        );
      case 'table':
        return _table(block);
      case 'separator':
        return const Padding(
          padding: EdgeInsets.symmetric(vertical: 8),
          child: Divider(height: 1, color: _line),
        );
      default:
        return Padding(
          padding: const EdgeInsets.symmetric(vertical: 3),
          child: SelectableText.rich(_span(block['runs'], _body)),
        );
    }
  }

  Widget _row(String marker, Object? runs) {
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 2),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          SizedBox(
            width: 22,
            child: Text(marker, style: _body.copyWith(color: _inkSoft)),
          ),
          Expanded(child: SelectableText.rich(_span(runs, _body))),
        ],
      ),
    );
  }

  Widget _table(Map<String, dynamic> block) {
    final headers = (block['headers'] as List?) ?? const [];
    final rows = (block['rows'] as List?) ?? const [];
    if (headers.isEmpty) {
      return const SizedBox.shrink();
    }
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 6),
      child: SingleChildScrollView(
        scrollDirection: Axis.horizontal,
        child: Table(
          border: TableBorder.all(color: _line, width: 1),
          defaultColumnWidth: const IntrinsicColumnWidth(),
          children: [
            TableRow(
              decoration: const BoxDecoration(color: _panel),
              children: [
                for (final header in headers)
                  _cell(header, bold: true),
              ],
            ),
            for (final row in rows.whereType<List>())
              TableRow(
                children: [
                  for (var index = 0; index < headers.length; index++)
                    _cell(index < row.length ? row[index] : null),
                ],
              ),
          ],
        ),
      ),
    );
  }

  Widget _cell(Object? runs, {bool bold = false}) {
    return Padding(
      padding: const EdgeInsets.symmetric(horizontal: 8, vertical: 6),
      child: SelectableText.rich(
        _span(
          runs,
          _body.copyWith(
            fontSize: 13.5,
            height: 1.35,
            fontWeight: bold ? FontWeight.w600 : FontWeight.w400,
          ),
        ),
      ),
    );
  }

  static TextSpan _span(Object? runs, TextStyle base) =>
      markdownSpan(runs, base);
}

class _ErrorBanner extends StatelessWidget {
  const _ErrorBanner({required this.message, required this.onDismiss});

  final String message;
  final VoidCallback onDismiss;

  @override
  Widget build(BuildContext context) {
    return Container(
      width: double.infinity,
      color: const Color(0xFF43201E),
      padding: const EdgeInsets.fromLTRB(14, 8, 6, 8),
      child: Row(
        children: [
          Expanded(
            child: Text(
              message,
              style: const TextStyle(color: Color(0xFFFFB4AB), fontSize: 13),
            ),
          ),
          IconButton(
            tooltip: 'Dismiss',
            onPressed: onDismiss,
            iconSize: 18,
            visualDensity: VisualDensity.compact,
            icon: const Icon(Icons.close, color: Color(0xFFFFB4AB)),
          ),
        ],
      ),
    );
  }
}

class _ClarifyCard extends StatelessWidget {
  const _ClarifyCard({required this.app});

  final AppState app;

  @override
  Widget build(BuildContext context) {
    return Container(
      width: double.infinity,
      margin: const EdgeInsets.fromLTRB(12, 0, 12, 8),
      padding: const EdgeInsets.all(14),
      decoration: BoxDecoration(
        color: _panel,
        borderRadius: BorderRadius.circular(14),
        border: Border.all(color: _accent.withValues(alpha: 0.5)),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          const Text(
            'The agent is asking',
            style: TextStyle(fontSize: 11, color: _inkFaint),
          ),
          const SizedBox(height: 6),
          Text(app.clarifyQuestion ?? '', style: const TextStyle(fontSize: 14)),
          const SizedBox(height: 12),
          Wrap(
            spacing: 8,
            runSpacing: 8,
            children: [
              for (final choice in app.clarifyChoices)
                FilledButton.tonal(
                  onPressed: () => app.answerClarify(choice),
                  child: Text(choice),
                ),
              TextButton(
                onPressed: () => app.answerClarify(''),
                child: const Text('Dismiss'),
              ),
            ],
          ),
        ],
      ),
    );
  }
}

class _Composer extends StatelessWidget {
  const _Composer({
    required this.controller,
    required this.app,
    required this.onSend,
  });

  final TextEditingController controller;
  final AppState app;
  final VoidCallback onSend;

  @override
  Widget build(BuildContext context) {
    return SafeArea(
      top: false,
      child: Container(
        padding: const EdgeInsets.fromLTRB(12, 8, 12, 8),
        decoration: const BoxDecoration(
          color: _panel,
          border: Border(top: BorderSide(color: _line)),
        ),
        child: Row(
          crossAxisAlignment: CrossAxisAlignment.end,
          children: [
            Expanded(
              child: TextField(
                controller: controller,
                minLines: 1,
                maxLines: 6,
                style: const TextStyle(fontSize: 15),
                decoration: InputDecoration(
                  hintText: 'Ask your agent for follow-up changes',
                  hintStyle: const TextStyle(color: _inkFaint),
                  filled: true,
                  fillColor: _raised,
                  contentPadding: const EdgeInsets.symmetric(
                    horizontal: 14,
                    vertical: 10,
                  ),
                  border: OutlineInputBorder(
                    borderRadius: BorderRadius.circular(20),
                    borderSide: const BorderSide(color: _line),
                  ),
                  enabledBorder: OutlineInputBorder(
                    borderRadius: BorderRadius.circular(20),
                    borderSide: const BorderSide(color: _line),
                  ),
                ),
              ),
            ),
            const SizedBox(width: 8),
            app.sending
                ? IconButton.filledTonal(
                    tooltip: 'Stop',
                    onPressed: app.interrupt,
                    icon: const Icon(Icons.stop_rounded),
                  )
                : IconButton.filled(
                    tooltip: 'Send',
                    onPressed: app.canSend ? onSend : null,
                    icon: const Icon(Icons.arrow_upward_rounded),
                  ),
          ],
        ),
      ),
    );
  }
}

class _SessionDrawer extends StatelessWidget {
  const _SessionDrawer({required this.app});

  final AppState app;

  @override
  Widget build(BuildContext context) {
    return Drawer(
      backgroundColor: _panel,
      child: SafeArea(
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 16, 16, 8),
              child: Text(
                'Sessions',
                style: TextStyle(
                  fontSize: 17,
                  fontWeight: FontWeight.w600,
                  color: _ink,
                ),
              ),
            ),
            const Divider(height: 1, color: _line),
            Expanded(
              // Pulling the list down refreshes it, the way every list on a
              // phone behaves; the refresh button in the appbar does the same
              // for when the drawer is closed.
              child: RefreshIndicator(
                onRefresh: app.refreshAll,
                backgroundColor: _panel,
                color: _accent,
                child: app.sessions.isEmpty
                    ? ListView(
                        physics: const AlwaysScrollableScrollPhysics(),
                        children: const [
                          Padding(
                            padding: EdgeInsets.all(16),
                            child: Text(
                              'No sessions yet.',
                              style: TextStyle(color: _inkFaint),
                            ),
                          ),
                        ],
                      )
                    : ListView.builder(
                        physics: const AlwaysScrollableScrollPhysics(),
                        itemCount: app.sessions.length,
                        itemBuilder: (context, index) {
                          final session = app.sessions[index];
                          return ListTile(
                            selected: session.id == app.sessionId,
                            selectedTileColor:
                                _accent.withValues(alpha: 0.12),
                            title: Text(
                              session.title,
                              maxLines: 1,
                              overflow: TextOverflow.ellipsis,
                              style: const TextStyle(fontSize: 14),
                            ),
                            subtitle: session.model == null
                                ? null
                                : Text(
                                    session.model!,
                                    maxLines: 1,
                                    overflow: TextOverflow.ellipsis,
                                    style: const TextStyle(
                                      fontSize: 11,
                                      color: _inkFaint,
                                    ),
                                  ),
                            onTap: () {
                              Navigator.of(context).pop();
                              app.openSession(session.id);
                            },
                          );
                        },
                      ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}

/// Connection settings. A sheet rather than a page: it is filled in once and
/// then rarely looked at.
void _openSettings(BuildContext context) {
  showModalBottomSheet<void>(
    context: context,
    isScrollControlled: true,
    backgroundColor: _panel,
    builder: (context) => const _SettingsSheet(),
  );
}

class _SettingsSheet extends StatefulWidget {
  const _SettingsSheet();

  @override
  State<_SettingsSheet> createState() => _SettingsSheetState();
}

class _SettingsSheetState extends State<_SettingsSheet> {
  final _app = AppState.instance;
  late final TextEditingController _host = TextEditingController(
    text: _app.host,
  );
  late final TextEditingController _port = TextEditingController(
    text: _app.port.toString(),
  );
  late final TextEditingController _token = TextEditingController(
    text: _app.credential,
  );
  late FontSize _fontSize = _app.fontSize;
  late bool _showToolCalls = _app.showToolCalls;

  @override
  void dispose() {
    _host.dispose();
    _port.dispose();
    _token.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final connected = _app.link == LinkState.online;
    return Padding(
      padding: EdgeInsets.only(
        left: 20,
        right: 20,
        top: 20,
        bottom: MediaQuery.of(context).viewInsets.bottom + 20,
      ),
      child: SingleChildScrollView(
        child: Column(
          mainAxisSize: MainAxisSize.min,
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            const Text(
              'Connection',
              style: TextStyle(fontSize: 18, fontWeight: FontWeight.w600),
            ),
            const SizedBox(height: 18),
            DropdownButtonFormField<Backend>(
              initialValue: _app.backend,
              decoration: const InputDecoration(labelText: 'Backend'),
              items: [
                for (final backend in Backend.values)
                  DropdownMenuItem(value: backend, child: Text(backend.label)),
              ],
              onChanged: (backend) {
                if (backend == null) {
                  return;
                }
                setState(() {
                  _app.backend = backend;
                  _port.text = backend.defaultPort.toString();
                  _app.useTls = backend.prefersTls;
                });
              },
            ),
            const SizedBox(height: 14),
            TextField(
              controller: _host,
              decoration: const InputDecoration(
                labelText: 'Host or URL',
                helperText: 'A full ws:// or wss:// URL overrides host and port.',
                helperMaxLines: 2,
              ),
            ),
            const SizedBox(height: 14),
            Row(
              children: [
                Expanded(
                  child: TextField(
                    controller: _port,
                    keyboardType: TextInputType.number,
                    decoration: const InputDecoration(labelText: 'Port'),
                  ),
                ),
                const SizedBox(width: 14),
                Expanded(
                  child: SwitchListTile(
                    contentPadding: EdgeInsets.zero,
                    title: const Text('TLS', style: TextStyle(fontSize: 14)),
                    value: _app.useTls,
                    onChanged: (value) => setState(() => _app.useTls = value),
                  ),
                ),
              ],
            ),
            const SizedBox(height: 6),
            TextField(
              controller: _token,
              obscureText: true,
              decoration: const InputDecoration(
                labelText: 'Token',
                helperText: 'Gateway token, session token or server password.',
                helperMaxLines: 2,
              ),
            ),
            const SizedBox(height: 22),
            Row(
              mainAxisAlignment: MainAxisAlignment.end,
              children: [
                if (connected)
                  TextButton(
                    onPressed: () {
                      _app.disconnect();
                      Navigator.of(context).pop();
                    },
                    child: const Text('Disconnect'),
                  ),
                const Spacer(),
                TextButton(
                  onPressed: () => Navigator.of(context).pop(),
                  child: const Text('Cancel'),
                ),
                const SizedBox(width: 8),
                FilledButton(
                  onPressed: () {
                    _app.host = _host.text.trim();
                    _app.port = int.tryParse(_port.text.trim()) ?? _app.port;
                    _app.credential = _token.text.trim();
                    _app.connect();
                    Navigator.of(context).pop();
                  },
                  child: Text(connected ? 'Reconnect' : 'Connect'),
                ),
              ],
            ),

            const SizedBox(height: 26),
            const Divider(height: 1, color: _line),
            const SizedBox(height: 18),
            const Text(
              'Interface',
              style: TextStyle(fontSize: 18, fontWeight: FontWeight.w600),
            ),
            const SizedBox(height: 14),
            DropdownButtonFormField<FontSize>(
              initialValue: _fontSize,
              decoration: const InputDecoration(
                labelText: 'Text size',
                helperText: 'Scales every font in the app at once.',
                helperMaxLines: 2,
              ),
              items: [
                for (final size in FontSize.values)
                  DropdownMenuItem(value: size, child: Text(size.label)),
              ],
              onChanged: (size) {
                if (size == null) {
                  return;
                }
                setState(() => _fontSize = size);
                _app.setFontSize(size);
              },
            ),
            const SizedBox(height: 6),
            SwitchListTile(
              contentPadding: EdgeInsets.zero,
              value: _showToolCalls,
              title: const Text(
                'Show tool calls',
                style: TextStyle(fontSize: 14),
              ),
              subtitle: const Text(
                'Off draws only the replies, with the tool calls a turn ran left out.',
                style: TextStyle(fontSize: 11, color: _inkFaint),
              ),
              onChanged: (value) {
                setState(() => _showToolCalls = value);
                _app.setShowToolCalls(value);
              },
            ),
            const SizedBox(height: 8),
          ],
        ),
      ),
    );
  }
}
