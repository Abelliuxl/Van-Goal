import 'package:flutter/services.dart';

class ChatAttachment {
  const ChatAttachment({
    required this.path,
    required this.name,
    this.remote = false,
  });
  factory ChatAttachment.fromJson(Map<String, dynamic> value) => ChatAttachment(
    path: value['path'] as String? ?? '',
    name: value['name'] as String? ?? 'Attachment',
    remote: value['remote'] == true,
  );
  final String path;
  final String name;
  final bool remote;
  bool get isImage =>
      RegExp(r'\.(png|jpe?g|gif|webp)$', caseSensitive: false).hasMatch(name);
}

/// Android owns content URIs and shared Downloads; Rust only receives private
/// file paths, never temporary grants that disappear while uploading.
class AndroidFiles {
  static const channel = MethodChannel('com.abelliuxl.van_goal/files');

  static Future<List<ChatAttachment>> pick({
    bool images = false,
    int maximum = 6,
  }) async {
    final items = await channel.invokeListMethod<dynamic>('pick', {
      'images': images,
      'maximum': maximum,
    });
    return (items ?? [])
        .map(
          (item) =>
              ChatAttachment.fromJson(Map<String, dynamic>.from(item as Map)),
        )
        .toList();
  }

  static Future<ChatAttachment> importImage(
    KeyboardInsertedContent content,
  ) async {
    final item = await channel.invokeMapMethod<String, dynamic>('importImage', {
      'uri': content.uri,
      'data': content.data,
      'mime': content.mimeType,
    });
    return ChatAttachment.fromJson(item!);
  }

  static Future<String?> save(String path, String name) =>
      channel.invokeMethod<String>('save', {'path': path, 'name': name});
  static Future<void> open(String uri, String name) =>
      channel.invokeMethod<void>('open', {'uri': uri, 'name': name});
}
