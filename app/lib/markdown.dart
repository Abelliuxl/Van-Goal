import 'package:flutter/material.dart';

/// The palette the transcript draws with, shared with the rest of the UI.
const markdownAccent = Color(0xFF4F8CFF);
const markdownInk = Color(0xFFE8E8EC);
const markdownSurface = Color(0xFF141416);
const markdownMono = 'monospace';

/// Turn the inline runs the Rust core handed over into styled spans.
///
/// The runs carry their own text rather than offsets into the message, which is
/// the point: an offset into a Rust `&str` counts bytes and a Dart `String`
/// counts UTF-16 units, so one emoji is enough to make the two disagree about
/// where a style starts. Nothing here has to count characters — it appends.
TextSpan markdownSpan(Object? runs, TextStyle base) {
  if (runs is! List || runs.isEmpty) {
    return TextSpan(text: '', style: base);
  }
  return TextSpan(
    style: base,
    children: [
      for (final run in runs.whereType<Map<String, dynamic>>())
        TextSpan(
          text: (run['text'] as String?) ?? '',
          style: markdownRunStyle(base, run),
        ),
    ],
  );
}

/// One run's styles, applied on top of the block's base style.
TextStyle markdownRunStyle(TextStyle base, Map<String, dynamic> run) {
  var style = base;
  for (final name in (run['styles'] as List?) ?? const []) {
    switch (name) {
      case 'strong':
        style = style.copyWith(fontWeight: FontWeight.w700);
      case 'emphasis':
        style = style.copyWith(fontStyle: FontStyle.italic);
      case 'strikethrough':
        style = style.copyWith(decoration: TextDecoration.lineThrough);
      case 'code':
        style = style.copyWith(
          fontFamily: markdownMono,
          fontSize: (style.fontSize ?? 15) - 1.5,
          color: markdownInk,
          backgroundColor: markdownSurface,
        );
    }
  }
  if (run['url'] != null) {
    style = style.copyWith(
      color: markdownAccent,
      decoration: TextDecoration.underline,
      decorationColor: markdownAccent,
    );
  }
  return style;
}
