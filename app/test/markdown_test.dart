import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:van_goal/markdown.dart';

/// One inline run, written out by hand the way `crates/mobile` serializes it.
Map<String, dynamic> run(
  String text, {
  List<String> styles = const [],
  String? url,
}) =>
    {'text': text, 'styles': styles, 'url': url};

void main() {
  const base = TextStyle(fontSize: 15);

  test('a strong run is bold and a plain one is not', () {
    final span = markdownSpan(
      [
        run('看你这台新设备到了：'),
        run('Van-Goal', styles: ['strong']),
      ],
      base,
    );
    final children = span.children!.cast<TextSpan>();
    expect(children.length, 2);
    expect(children[0].text, '看你这台新设备到了：');
    expect(children[0].style!.fontWeight, isNot(FontWeight.w700));
    expect(children[1].text, 'Van-Goal');
    // The delimiters are gone: what is drawn is the text, not the asterisks.
    expect(children[1].style!.fontWeight, FontWeight.w700);
    expect(
      children.map((child) => child.text).join(),
      isNot(contains('*')),
      reason: 'the markdown delimiters must not reach the screen',
    );
  });

  test('a code run is drawn in the monospace face, smaller than the body', () {
    final span = markdownSpan([run('0556c967', styles: ['code'])], base);
    final child = span.children!.single as TextSpan;
    expect(child.style!.fontFamily, markdownMono);
    expect(child.style!.fontSize, lessThan(base.fontSize!));
  });

  test('a link is coloured and underlined', () {
    final span = markdownSpan(
      [run('这里', url: 'https://example.com')],
      base,
    );
    final child = span.children!.single as TextSpan;
    expect(child.style!.color, markdownAccent);
    expect(child.style!.decoration, TextDecoration.underline);
  });

  test('emphasis and strikethrough carry their own styles', () {
    final span = markdownSpan([
      run('斜', styles: ['emphasis']),
      run('删', styles: ['strikethrough']),
    ], base);
    final children = span.children!.cast<TextSpan>();
    expect(children[0].style!.fontStyle, FontStyle.italic);
    expect(children[1].style!.decoration, TextDecoration.lineThrough);
  });

  test('nested styles stack instead of replacing one another', () {
    final span = markdownSpan([
      run('又粗又斜', styles: ['strong', 'emphasis']),
    ], base);
    final child = span.children!.single as TextSpan;
    expect(child.style!.fontWeight, FontWeight.w700);
    expect(child.style!.fontStyle, FontStyle.italic);
  });

  test('emoji survive: runs carry their own text, so nothing is sliced', () {
    final span = markdownSpan([
      run('你好 👋❄️ '),
      run('粗体', styles: ['strong']),
      run(' 结尾'),
    ], base);
    final text = span.children!.cast<TextSpan>().map((c) => c.text).join();
    expect(text, '你好 👋❄️ 粗体 结尾');
    expect((span.children![1] as TextSpan).style!.fontWeight, FontWeight.w700);
  });

  test('a message with no runs draws nothing rather than throwing', () {
    expect(markdownSpan(null, base).text, '');
    expect(markdownSpan(const [], base).text, '');
  });
}
