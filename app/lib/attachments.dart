import 'dart:io';

import 'package:flutter/material.dart';

import 'files.dart';

class AttachmentView extends StatefulWidget {
  const AttachmentView({
    super.key,
    required this.item,
    required this.load,
    this.onRemove,
  });
  final ChatAttachment item;
  final Future<String> Function(ChatAttachment) load;
  final VoidCallback? onRemove;
  @override
  State<AttachmentView> createState() => _AttachmentViewState();
}

class _AttachmentViewState extends State<AttachmentView> {
  Future<String>? _image;
  bool _saving = false;
  @override
  void initState() {
    super.initState();
    if (widget.item.isImage) _image = widget.load(widget.item);
  }

  @override
  void didUpdateWidget(covariant AttachmentView old) {
    super.didUpdateWidget(old);
    if (old.item.path != widget.item.path) {
      _image = widget.item.isImage ? widget.load(widget.item) : null;
    }
  }

  Future<void> _save() async {
    setState(() => _saving = true);
    try {
      final path = await widget.load(widget.item);
      final uri = await AndroidFiles.save(path, widget.item.name);
      if (!mounted || uri == null) return;
      ScaffoldMessenger.of(context).showSnackBar(
        SnackBar(
          content: const Text('文件已保存'),
          action: SnackBarAction(
            label: '打开',
            onPressed: () async {
              try {
                await AndroidFiles.open(uri, widget.item.name);
              } on Object catch (error) {
                if (mounted) _error(error);
              }
            },
          ),
        ),
      );
    } on Object catch (error) {
      if (mounted) _error(error);
    } finally {
      if (mounted) setState(() => _saving = false);
    }
  }

  void _error(Object error) =>
      ScaffoldMessenger.of(context)
          .showSnackBar(SnackBar(content: Text(error.toString())));

  @override
  Widget build(BuildContext context) {
    final compact = widget.onRemove != null;
    return Padding(
      padding: const EdgeInsets.only(bottom: 6),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        mainAxisSize: MainAxisSize.min,
        children: [
          if (_image != null)
            FutureBuilder<String>(
              future: _image,
              builder: (context, result) {
                if (result.hasError) {
                  return TextButton.icon(
                    onPressed: () =>
                        setState(() => _image = widget.load(widget.item)),
                    icon: const Icon(Icons.refresh, size: 16),
                    label: const Text('重试预览'),
                  );
                }
                if (!result.hasData) {
                  return const Padding(
                    padding: EdgeInsets.all(8),
                    child: SizedBox(
                      width: 18,
                      height: 18,
                      child: CircularProgressIndicator(strokeWidth: 2),
                    ),
                  );
                }
                final path = result.data!;
                return LayoutBuilder(
                  builder: (context, constraints) {
                    final previewWidth = constraints.maxWidth
                        .clamp(0.0, compact ? 120.0 : 420.0)
                        .toDouble();
                    return InkWell(
                      onTap: compact
                          ? null
                          : () => showDialog<void>(
                              context: context,
                              builder: (context) => Dialog(
                                insetPadding: const EdgeInsets.all(16),
                                constraints: const BoxConstraints(
                                  maxWidth: 1000,
                                ),
                                child: SizedBox(
                                  width: double.infinity,
                                  height:
                                      MediaQuery.sizeOf(context).height * 0.8,
                                  child: Stack(
                                    children: [
                                      Positioned.fill(
                                        child: InteractiveViewer(
                                          child: Center(
                                            child: Image.file(
                                              File(path),
                                              cacheWidth: 2000,
                                              fit: BoxFit.contain,
                                            ),
                                          ),
                                        ),
                                      ),
                                      Positioned(
                                        right: 0,
                                        top: 0,
                                        child: IconButton(
                                          onPressed: () =>
                                              Navigator.pop(context),
                                          icon: const Icon(Icons.close),
                                        ),
                                      ),
                                    ],
                                  ),
                                ),
                              ),
                            ),
                      child: ClipRRect(
                        borderRadius: BorderRadius.circular(8),
                        child: Image.file(
                          File(path),
                          width: previewWidth,
                          height: compact
                              ? 70
                              : (previewWidth * 0.65).clamp(0, 260).toDouble(),
                          cacheWidth: 960,
                          fit: BoxFit.contain,
                          errorBuilder: (_, _, _) => const Text('无法预览此图片'),
                        ),
                      ),
                    );
                  },
                );
              },
            ),
          if (compact)
            InputChip(
              label: Text(
                widget.item.name,
                maxLines: 1,
                overflow: TextOverflow.ellipsis,
              ),
              onDeleted: widget.onRemove,
            )
          else
            TextButton.icon(
              onPressed: _saving ? null : _save,
              icon: _saving
                  ? const SizedBox(
                      width: 16,
                      height: 16,
                      child: CircularProgressIndicator(strokeWidth: 2),
                    )
                  : const Icon(Icons.download_outlined, size: 18),
              label: Text(
                widget.item.name,
                maxLines: 2,
                overflow: TextOverflow.ellipsis,
              ),
            ),
        ],
      ),
    );
  }
}
