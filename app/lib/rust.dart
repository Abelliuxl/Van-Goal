import 'dart:convert';
import 'dart:ffi';
import 'dart:io';

import 'package:ffi/ffi.dart';

/// The three functions `crates/mobile` exports, plus the data-directory call.
///
/// The C side speaks JSON in both directions: a command goes in as a string, an
/// acknowledgement comes back, and everything the command actually produced
/// arrives later from [poll]. Nothing here blocks on the network — see the
/// module comment in `crates/mobile/src/lib.rs` for why that matters.
class RustCore {
  RustCore._(DynamicLibrary library)
      : _command = library.lookupFunction<
            Pointer<Utf8> Function(Pointer<Utf8>),
            Pointer<Utf8> Function(Pointer<Utf8>)>('vg_command'),
        _poll = library.lookupFunction<Pointer<Utf8> Function(),
            Pointer<Utf8> Function()>('vg_poll'),
        _free = library.lookupFunction<Void Function(Pointer<Utf8>),
            void Function(Pointer<Utf8>)>('vg_free'),
        _setDataDir = library.lookupFunction<
            Pointer<Utf8> Function(Pointer<Utf8>),
            Pointer<Utf8> Function(Pointer<Utf8>)>('vg_set_data_dir');

  static RustCore? _instance;

  /// The one connection this app has, opened on first use.
  static RustCore get instance => _instance ??= RustCore._(_open());

  final Pointer<Utf8> Function(Pointer<Utf8>) _command;
  final Pointer<Utf8> Function() _poll;
  final void Function(Pointer<Utf8>) _free;
  final Pointer<Utf8> Function(Pointer<Utf8>) _setDataDir;

  static DynamicLibrary _open() {
    if (Platform.isAndroid) {
      return DynamicLibrary.open('libvan_goal_mobile.so');
    }
    if (Platform.isIOS) {
      // Statically linked into the app binary, so the symbols are already in
      // the process rather than in a separate library.
      return DynamicLibrary.process();
    }
    if (Platform.isMacOS) {
      return DynamicLibrary.open('libvan_goal_mobile.dylib');
    }
    throw UnsupportedError('Van-Goal has no Rust library for ${Platform.operatingSystem}');
  }

  /// Tell the Rust side where it may keep settings and its session cache.
  ///
  /// Must be the first call made: the directory is chosen once, and the reply
  /// says whether this call was the one that chose it.
  bool setDataDir(String path) {
    final input = path.toNativeUtf8(allocator: malloc);
    try {
      return _decode(_take(_setDataDir(input)))['ok'] == true;
    } finally {
      malloc.free(input);
    }
  }

  /// Send one command. The map carries a `cmd` key and its arguments.
  Map<String, dynamic> send(Map<String, dynamic> payload) {
    final input = jsonEncode(payload).toNativeUtf8(allocator: malloc);
    try {
      return _decode(_take(_command(input)));
    } finally {
      malloc.free(input);
    }
  }

  /// Take everything that has happened since the previous call.
  List<Map<String, dynamic>> poll() {
    final decoded = jsonDecode(_take(_poll()));
    if (decoded is! List) {
      return const [];
    }
    return decoded.whereType<Map<String, dynamic>>().toList(growable: false);
  }

  /// Copy a string out of Rust's memory and hand the buffer straight back —
  /// the library allocated it and only the library may release it.
  String _take(Pointer<Utf8> pointer) {
    if (pointer == nullptr) {
      return '';
    }
    final text = pointer.toDartString();
    _free(pointer);
    return text;
  }

  Map<String, dynamic> _decode(String text) {
    final decoded = jsonDecode(text);
    return decoded is Map<String, dynamic> ? decoded : <String, dynamic>{};
  }
}
