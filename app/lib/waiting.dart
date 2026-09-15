import 'dart:math' as math;

import 'package:flutter/material.dart';

import 'state.dart';

const _inkSoft = Color(0xFF9B9BA6);

/// How strongly one dot of the waiting indicator is lit at a point in its cycle,
/// as a number from 0 to 1.
///
/// The dots are staggered so the row reads as a wave travelling left to right,
/// which is what catches the eye. Three dots brightening together would be a
/// blink, and a blink reads as a rendering fault rather than as work in
/// progress. A full cycle takes [WaitingDots.period].
double typingDotPulse(double cycle, int index, {int count = 3}) {
  final phase = (cycle + index * (1 / (count * 2))) % 1.0;
  return 0.5 - 0.5 * math.cos(2 * math.pi * phase);
}

/// The three dots shown while a reply is on its way.
///
/// A wait with no movement is indistinguishable from a screen that has stopped
/// updating, which is exactly how an empty bubble reads: the user is left
/// wondering whether the app is working or stuck.
class WaitingDots extends StatefulWidget {
  const WaitingDots({super.key});

  /// One full wave. Slow enough to read as breathing, fast enough that nobody
  /// looks twice to check it is moving.
  static const period = Duration(milliseconds: 1100);

  @override
  State<WaitingDots> createState() => _WaitingDotsState();
}

class _WaitingDotsState extends State<WaitingDots>
    with SingleTickerProviderStateMixin {
  late final AnimationController _controller = AnimationController(
    vsync: this,
    duration: WaitingDots.period,
  )..repeat();

  @override
  void dispose() {
    _controller.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return AnimatedBuilder(
      animation: _controller,
      builder: (context, _) => Row(
        mainAxisSize: MainAxisSize.min,
        children: [
          for (var index = 0; index < 3; index++)
            Padding(
              padding: EdgeInsets.only(right: index == 2 ? 0 : 6),
              child: _dot(index),
            ),
        ],
      ),
    );
  }

  Widget _dot(int index) {
    final pulse = typingDotPulse(_controller.value, index);
    return Opacity(
      key: ValueKey('typing-dot-$index'),
      // Never fully transparent: a dot that disappears makes the row flicker,
      // and a dim dot is enough to show how many are coming.
      opacity: 0.3 + 0.7 * pulse,
      child: Transform.translate(
        offset: Offset(0, -1.5 * pulse),
        child: Container(
          width: 7,
          height: 7,
          decoration: const BoxDecoration(
            color: _inkSoft,
            shape: BoxShape.circle,
          ),
        ),
      ),
    );
  }
}

/// The row of messages plus the bubble a reply is about to appear in.
///
/// The gap this fills is the one between a prompt being sent and the gateway
/// saying anything at all: until the first event arrives there is no assistant
/// message to draw, so without this the screen shows the user's own message and
/// then nothing, for as long as the agent takes to start.
List<Message> withWaitingBubble(List<Message> messages, bool sending) {
  final waiting = sending && (messages.isEmpty || messages.last.fromUser);
  if (!waiting) {
    return messages;
  }
  return [
    ...messages,
    Message(id: 'waiting-for-reply', text: '', fromUser: false, streaming: true),
  ];
}
