import 'package:flutter/material.dart';

import 'state.dart';

/// The palette the transcript draws with, shared with the rest of the UI.
const _surface = Color(0xFF141416);
const _panel = Color(0xFF1A1A1E);
const _line = Color(0xFF2C2C33);
const _inkSoft = Color(0xFF9B9BA6);
const _inkFaint = Color(0xFF6D6D78);
const _mono = 'monospace';

/// One turn's tool activity, folded into a single row.
///
/// A turn can run dozens of calls, and a gateway reports each one twice — once
/// when it starts and once when it finishes — so drawn one per event they bury
/// the reply they belong to. The count and the tools used are what a reader
/// wants at a glance; the calls themselves are one tap away.
class ToolCallsView extends StatefulWidget {
  const ToolCallsView({super.key, required this.tools});

  final List<ToolCall> tools;

  @override
  State<ToolCallsView> createState() => _ToolCallsViewState();
}

class _ToolCallsViewState extends State<ToolCallsView> {
  bool _open = false;

  /// The tools used, in the order they were first used and without repeats.
  List<String> get _names {
    final names = <String>[];
    for (final tool in widget.tools) {
      if (!names.contains(tool.name)) {
        names.add(tool.name);
      }
    }
    return names;
  }

  @override
  Widget build(BuildContext context) {
    final count = widget.tools.length;
    final summary =
        '$count tool ${count == 1 ? 'call' : 'calls'} · ${_names.join(', ')}';
    return Container(
      width: double.infinity,
      decoration: BoxDecoration(
        color: _panel,
        borderRadius: BorderRadius.circular(10),
        border: Border.all(color: _line),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          InkWell(
            borderRadius: BorderRadius.circular(10),
            onTap: () => setState(() => _open = !_open),
            child: Padding(
              padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 8),
              child: Row(
                children: [
                  const Icon(Icons.build_outlined, size: 14, color: _inkFaint),
                  const SizedBox(width: 8),
                  Expanded(
                    child: Text(
                      summary,
                      maxLines: 1,
                      overflow: TextOverflow.ellipsis,
                      style: const TextStyle(fontSize: 12, color: _inkSoft),
                    ),
                  ),
                  Icon(
                    _open ? Icons.expand_less : Icons.expand_more,
                    size: 16,
                    color: _inkFaint,
                  ),
                ],
              ),
            ),
          ),
          if (_open)
            Padding(
              padding: const EdgeInsets.fromLTRB(10, 0, 10, 8),
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                mainAxisSize: MainAxisSize.min,
                children: [
                  for (final tool in widget.tools) ToolCallChip(tool: tool),
                ],
              ),
            ),
        ],
      ),
    );
  }
}

/// One call: what it was, how far it got, and whatever the gateway said about
/// it.
class ToolCallChip extends StatelessWidget {
  const ToolCallChip({super.key, required this.tool});

  final ToolCall tool;

  @override
  Widget build(BuildContext context) {
    final detail = tool.detail.trim();
    return Container(
      width: double.infinity,
      margin: const EdgeInsets.only(top: 6),
      padding: const EdgeInsets.symmetric(horizontal: 10, vertical: 6),
      decoration: BoxDecoration(
        color: _surface,
        borderRadius: BorderRadius.circular(8),
        border: Border.all(color: _line),
      ),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          Row(
            children: [
              Expanded(
                child: Text(
                  tool.name,
                  maxLines: 1,
                  overflow: TextOverflow.ellipsis,
                  style: const TextStyle(fontSize: 12, color: _inkSoft),
                ),
              ),
              if (tool.status.isNotEmpty)
                Text(
                  tool.status,
                  style: const TextStyle(fontSize: 11, color: _inkFaint),
                ),
            ],
          ),
          if (detail.isNotEmpty)
            Padding(
              padding: const EdgeInsets.only(top: 2),
              child: Text(
                detail,
                maxLines: 4,
                overflow: TextOverflow.ellipsis,
                style: const TextStyle(
                  fontSize: 11,
                  height: 1.35,
                  color: _inkFaint,
                  fontFamily: _mono,
                ),
              ),
            ),
        ],
      ),
    );
  }
}
