import 'package:flutter/material.dart';

import 'state.dart';

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

class VanGoalApp extends StatefulWidget {
  const VanGoalApp({super.key});

  @override
  State<VanGoalApp> createState() => _VanGoalAppState();
}

class _VanGoalAppState extends State<VanGoalApp> {
  @override
  void initState() {
    super.initState();
    // Naming the data directory has to happen before anything reads it, so the
    // client is started here rather than lazily from the first screen that
    // needs it.
    AppState.instance.start();
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
            Text(
              _linkLabel(_app),
              style: TextStyle(
                fontSize: 11,
                color: _app.link == LinkState.online ? _accent : _inkFaint,
              ),
            ),
          ],
        ),
        actions: [
          IconButton(
            tooltip: 'New chat',
            onPressed: _app.newChat,
            icon: const Icon(Icons.add_comment_outlined),
          ),
          Builder(
            builder: (context) => IconButton(
              tooltip: 'Sessions',
              onPressed: () => Scaffold.of(context).openDrawer(),
              icon: const Icon(Icons.forum_outlined),
            ),
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
          if (_app.error != null) _ErrorBanner(message: _app.error!),
          Expanded(child: _Transcript(app: _app, controller: _scroll)),
          if (_app.clarifyQuestion != null) _ClarifyCard(app: _app),
          _Composer(controller: _composer, app: _app, onSend: _send),
        ],
      ),
    );
  }

  static String _linkLabel(AppState app) {
    switch (app.link) {
      case LinkState.online:
        return '${app.backend.label} · connected';
      case LinkState.connecting:
        return '${app.backend.label} · connecting…';
      case LinkState.failed:
        return '${app.backend.label} · ${app.linkDetail ?? 'failed'}';
      case LinkState.offline:
        return '${app.backend.label} · offline';
    }
  }
}

class _Transcript extends StatelessWidget {
  const _Transcript({required this.app, required this.controller});

  final AppState app;
  final ScrollController controller;

  @override
  Widget build(BuildContext context) {
    if (app.messages.isEmpty) {
      return _EmptyState(app: app);
    }
    return ListView.builder(
      controller: controller,
      padding: const EdgeInsets.fromLTRB(12, 12, 12, 4),
      itemCount: app.messages.length,
      itemBuilder: (context, index) => _Bubble(message: app.messages[index]),
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
          ],
        ),
      ),
    );
  }
}

class _Bubble extends StatelessWidget {
  const _Bubble({required this.message});

  final Message message;

  @override
  Widget build(BuildContext context) {
    final isUser = message.fromUser;
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
            for (final tool in message.tools) _ToolChip(tool: tool),
            if (message.tools.isNotEmpty) const SizedBox(height: 8),
            if (message.text.isEmpty && message.streaming)
              const _Thinking()
            else
              SelectableText(
                message.text,
                style: const TextStyle(fontSize: 15, height: 1.45, color: _ink),
              ),
          ],
        ),
      ),
    );
  }
}

class _Thinking extends StatelessWidget {
  const _Thinking();

  @override
  Widget build(BuildContext context) {
    return const Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        SizedBox(
          width: 12,
          height: 12,
          child: CircularProgressIndicator(strokeWidth: 2),
        ),
        SizedBox(width: 10),
        Text('Thinking…', style: TextStyle(color: _inkSoft, fontSize: 14)),
      ],
    );
  }
}

class _ToolChip extends StatelessWidget {
  const _ToolChip({required this.tool});

  final ToolCall tool;

  @override
  Widget build(BuildContext context) {
    final detail = tool.detail.trim();
    return Container(
      width: double.infinity,
      margin: const EdgeInsets.only(bottom: 4),
      padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 6),
      decoration: BoxDecoration(
        color: _panel,
        borderRadius: BorderRadius.circular(8),
        border: Border.all(color: _line),
      ),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          const Icon(Icons.build_outlined, size: 13, color: _inkFaint),
          const SizedBox(width: 6),
          Expanded(
            child: Text(
              detail.isEmpty ? tool.name : '${tool.name} · $detail',
              maxLines: 3,
              overflow: TextOverflow.ellipsis,
              style: const TextStyle(fontSize: 12, color: _inkSoft),
            ),
          ),
        ],
      ),
    );
  }
}

class _ErrorBanner extends StatelessWidget {
  const _ErrorBanner({required this.message});

  final String message;

  @override
  Widget build(BuildContext context) {
    return Container(
      width: double.infinity,
      color: const Color(0xFF43201E),
      padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 10),
      child: Text(
        message,
        style: const TextStyle(color: Color(0xFFFFB4AB), fontSize: 13),
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
              padding: const EdgeInsets.fromLTRB(16, 16, 8, 8),
              child: Row(
                children: [
                  const Text(
                    'Sessions',
                    style: TextStyle(fontSize: 17, fontWeight: FontWeight.w600),
                  ),
                  const Spacer(),
                  IconButton(
                    tooltip: 'Refresh',
                    onPressed: app.refreshSessions,
                    icon: const Icon(Icons.refresh, size: 20),
                  ),
                ],
              ),
            ),
            const Divider(height: 1, color: _line),
            Expanded(
              child: app.sessions.isEmpty
                  ? const Padding(
                      padding: EdgeInsets.all(16),
                      child: Text(
                        'No sessions yet.',
                        style: TextStyle(color: _inkFaint),
                      ),
                    )
                  : ListView.builder(
                      itemCount: app.sessions.length,
                      itemBuilder: (context, index) {
                        final session = app.sessions[index];
                        return ListTile(
                          selected: session.id == app.sessionId,
                          selectedTileColor: _accent.withValues(alpha: 0.12),
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

  @override
  void dispose() {
    _host.dispose();
    _port.dispose();
    _token.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
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
                  child: const Text('Connect'),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }
}
