import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:van_goal/attachments.dart';
import 'package:van_goal/files.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  testWidgets(
    'an attachment-only reply can be saved with its original filename',
    (tester) async {
      final calls = <MethodCall>[];
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
          .setMockMethodCallHandler(AndroidFiles.channel, (call) async {
            calls.add(call);
            return call.method == 'save' ? 'content://downloads/fixture' : null;
          });
      addTearDown(
        () => TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
            .setMockMethodCallHandler(AndroidFiles.channel, null),
      );
      const item = ChatAttachment(
        path: '/api/download?token=fixture',
        name: '报告.pdf',
        remote: true,
      );
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: AttachmentView(
              item: item,
              load: (_) async => '/private/cached-file',
            ),
          ),
        ),
      );
      expect(find.text('报告.pdf'), findsOneWidget);
      await tester.tap(find.text('报告.pdf'));
      await tester.pumpAndSettle();
      expect(calls.single.method, 'save');
      expect(calls.single.arguments, {
        'path': '/private/cached-file',
        'name': '报告.pdf',
      });
      expect(find.text('文件已保存'), findsOneWidget);
      await tester.tap(find.text('打开'));
      await tester.pumpAndSettle();
      expect(calls.last.method, 'open');
    },
  );

  testWidgets(
    'a failed download can be attempted again without reporting success',
    (tester) async {
      var loads = 0;
      await tester.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: AttachmentView(
              item: const ChatAttachment(
                path: '/api/download',
                name: 'test.txt',
                remote: true,
              ),
              load: (_) async {
                loads++;
                throw StateError('HTTP 404');
              },
            ),
          ),
        ),
      );
      await tester.tap(find.text('test.txt'));
      await tester.pumpAndSettle();
      expect(find.text('文件已保存'), findsNothing);
      expect(find.textContaining('HTTP 404'), findsOneWidget);
      await tester.tap(find.text('test.txt'));
      await tester.pumpAndSettle();
      expect(loads, 2);
    },
  );
}
