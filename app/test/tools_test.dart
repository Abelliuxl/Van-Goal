import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:van_goal/state.dart';
import 'package:van_goal/tools.dart';

ToolCall call(String name, String status, [String detail = '']) =>
    ToolCall(name: name, status: status, detail: detail);

Widget host(Widget child) => MaterialApp(
      home: Scaffold(body: Column(children: [child])),
    );

void main() {
  testWidgets('a turn that ran six tools draws one row, not six', (tester) async {
    // What the core hands over for a turn that ran three tools, each reported
    // when it started and when it finished. Drawn one per event this was the
    // wall of chips the app used to show.
    final tools = [
      call('read', 'running', '{"path":"a.rs"}'),
      call('read', 'ok', '{"path":"a.rs"}'),
      call('bash', 'running', '{"cmd":"ls"}'),
      call('bash', 'ok', '{"cmd":"ls"}'),
      call('grep', 'running', '{"pattern":"x"}'),
      call('grep', 'ok', '{"pattern":"x"}'),
    ];
    await tester.pumpWidget(host(ToolCallsView(tools: tools)));

    expect(find.text('6 tool calls · read, bash, grep'), findsOneWidget);
    // Collapsed: none of the individual calls is drawn yet.
    expect(find.text('read'), findsNothing);
    expect(find.byType(ToolCallChip), findsNothing);
  });

  testWidgets('tapping the row opens the calls it is summarising', (tester) async {
    final tools = [
      call('read', 'ok', '{"path":"a.rs"}'),
      call('bash', 'running', ''),
    ];
    await tester.pumpWidget(host(ToolCallsView(tools: tools)));

    await tester.tap(find.text('2 tool calls · read, bash'));
    await tester.pumpAndSettle();

    expect(find.byType(ToolCallChip), findsNWidgets(2));
    expect(find.text('read'), findsOneWidget);
    expect(find.text('ok'), findsOneWidget);
    expect(find.text('{"path":"a.rs"}'), findsOneWidget);

    // And closes again.
    await tester.tap(find.text('2 tool calls · read, bash'));
    await tester.pumpAndSettle();
    expect(find.byType(ToolCallChip), findsNothing);
  });

  testWidgets('one call is described in the singular', (tester) async {
    await tester.pumpWidget(host(ToolCallsView(tools: [call('read', 'ok')])));
    expect(find.text('1 tool call · read'), findsOneWidget);
    expect(find.text('1 tool calls · read'), findsNothing);
  });

  testWidgets('the same tool twice is named once in the summary', (tester) async {
    final tools = [call('read', 'ok', 'a'), call('read', 'ok', 'b')];
    await tester.pumpWidget(host(ToolCallsView(tools: tools)));
    expect(find.text('2 tool calls · read'), findsOneWidget);
  });
}
