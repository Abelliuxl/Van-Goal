import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:van_goal/state.dart';
import 'package:van_goal/waiting.dart';

Message user(String text) =>
    Message(id: 'u', text: text, fromUser: true);

Message assistant(String text, {bool streaming = false}) =>
    Message(id: 'a', text: text, fromUser: false, streaming: streaming);

void main() {
  group('the pulse', () {
    test('stays between nothing and everything', () {
      for (var step = 0; step < 40; step++) {
        final cycle = step / 40;
        for (var index = 0; index < 3; index++) {
          final pulse = typingDotPulse(cycle, index);
          expect(pulse, inInclusiveRange(0.0, 1.0));
        }
      }
    });

    test('the dots never all light at once', () {
      // Three dots brightening together is a blink, and a blink reads as a
      // rendering fault. The two outer dots matching each other is expected —
      // they sit either side of the one at the top of the wave — so the rule is
      // that the row always has some contrast in it, not that all three differ.
      for (var step = 0; step < 40; step++) {
        final cycle = step / 40;
        final pulses = [0, 1, 2].map((i) => typingDotPulse(cycle, i)).toList();
        expect(
          pulses.reduce((a, b) => a > b ? a : b) -
              pulses.reduce((a, b) => a < b ? a : b),
          greaterThan(0.01),
          reason: 'the dots were flat at cycle $cycle',
        );
      }
    });

    test('a cycle brings every dot back to where it started', () {
      for (var index = 0; index < 3; index++) {
        expect(
          typingDotPulse(0, index),
          closeTo(typingDotPulse(1, index), 1e-9),
        );
      }
    });
  });

  testWidgets('the waiting dots move', (tester) async {
    await tester.pumpWidget(const MaterialApp(home: WaitingDots()));

    double dot(int index) => tester
        .widget<Opacity>(find.byKey(ValueKey('typing-dot-$index')))
        .opacity;

    final first = [dot(0), dot(1), dot(2)];
    await tester.pump(WaitingDots.period ~/ 4);
    final later = [dot(0), dot(1), dot(2)];

    // The whole point of the indicator: something is visibly happening.
    expect(later, isNot(equals(first)));
    // And it is never a blank row.
    expect(later.every((opacity) => opacity > 0), isTrue);
  });

  group('the waiting bubble', () {
    test('appears while a prompt is out with no reply started', () {
      final shown = withWaitingBubble([user('你好')], true);
      expect(shown.length, 2);
      expect(shown.last.fromUser, isFalse);
      expect(shown.last.streaming, isTrue);

      // An empty chat that is waiting still gets one.
      expect(withWaitingBubble(const [], true).length, 1);
    });

    test('stays away when nothing is pending', () {
      expect(withWaitingBubble([user('你好')], false).length, 1);
      expect(withWaitingBubble(const [], false), isEmpty);
    });

    test('gives way to the reply once one has started', () {
      // The assistant placeholder is already the last message: adding another
      // bubble would put a second, empty one under the reply being written.
      final messages = [user('你好'), assistant('', streaming: true)];
      expect(withWaitingBubble(messages, true), same(messages));

      final answered = [user('你好'), assistant('你好呀')];
      expect(withWaitingBubble(answered, true), same(answered));
    });
  });
}
