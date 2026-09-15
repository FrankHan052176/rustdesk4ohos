import 'dart:async';

import 'package:flutter/widgets.dart';



/// One instance per Dart isolate, never shared with the host's main window.
class OhosSessionWindow {
  static String? windowId;
  static String? connectionToken;
  static bool get isSession => windowId != null;
  static final ready = Completer<void>();
  static final closing = ValueNotifier<bool>(false);
  static final _sessions = <String, Future<void> Function()>{};
  static final _disposals = <Future<void>>[];
  static Future<void>? _closeFuture;

  static void registerSession(String id, Future<void> Function() close) {
    if (isSession) _sessions[id] = close;
  }

  static void trackDisposal(Future<void> future) {
    if (isSession) {
      // Observe failures immediately; close() still receives the original error.
      unawaited(future.then<void>((_) {}, onError: (Object _, StackTrace __) {}));
      _disposals.add(future);
    }
  }

  static Future<void> close() => _closeFuture ??= _close();

  static Future<void> _close() async {
    await ready.future;
    closing.value = true;
    // Removing the navigator unmounts every session page before engine teardown.
    await WidgetsBinding.instance.endOfFrame;
    final pending = <Future<void>>[
      ..._disposals,
      for (final close in _sessions.values) close(),
    ];
    _sessions.clear();
    _disposals.clear();
    connectionToken = null;
    // Every owner still finishes; the first failure keeps the native close vetoed.
    Object? failure;
    for (final task in pending) {
      try {
        await task;
      } catch (error) {
        failure ??= error;
      }
    }
    if (failure != null) {
      throw StateError('HarmonyOS session cleanup failed: $failure');
    }
  }
}

