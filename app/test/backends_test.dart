import 'package:flutter_test/flutter_test.dart';
import 'package:van_goal/state.dart';

void main() {
  test('the backends that need a local process are not offered', () {
    // Codex CLI, Claude Code and Pi are subprocesses, and a managed
    // `hermes serve` is launched as one. None of those binaries exist on a
    // phone, and `crates/mobile` refuses them — a list that offered them would
    // produce a connection that fails for a reason the user cannot act on.
    const unavailable = ['codex', 'claudecode', 'pi'];
    for (final id in unavailable) {
      expect(
        Backend.values.any((backend) => backend.id == id),
        isFalse,
        reason: '$id cannot run on a phone and must not be listed',
      );
    }
  });

  test('every backend the app offers has a distinct id and a port', () {
    final ids = Backend.values.map((backend) => backend.id).toSet();
    expect(ids.length, Backend.values.length);
    for (final backend in Backend.values) {
      expect(backend.defaultPort, greaterThan(0));
      expect(backend.label, isNotEmpty);
    }
  });

  test('an unknown or missing backend id falls back to one that exists', () {
    // The saved settings come from a file, so they may name a backend this
    // build does not have — an older one, or a desktop-only one.
    expect(Backend.fromId('openclaw'), Backend.openclaw);
    expect(Backend.fromId(null), Backend.openclaw);
    expect(Backend.fromId('claudecode'), Backend.openclaw);
  });

  test('only OpenClaw expects TLS by default', () {
    // It is the one backend normally reached over the public internet; the
    // others default to a loopback server.
    for (final backend in Backend.values) {
      expect(backend.prefersTls, backend == Backend.openclaw);
    }
  });
}
