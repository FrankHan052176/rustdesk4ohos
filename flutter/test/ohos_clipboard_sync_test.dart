import 'dart:async';

import 'package:flutter_hbb/models/ohos_clipboard_sync.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  late OhosClipboardSync sync;
  late List<String> sent;
  late List<String> written;
  late List<bool> forcedReads;
  String? local;
  String? remote;
  var reads = 0;
  var takes = 0;
  var accept = true;

  setUp(() {
    sent = [];
    written = [];
    forcedReads = [];
    local = null;
    remote = null;
    reads = 0;
    takes = 0;
    accept = true;
    sync = OhosClipboardSync(
      readLocal: (force) async {
        reads++;
        forcedReads.add(force);
        return local;
      },
      writeLocal: (text) async {
        written.add(text);
        local = text;
      },
      sendLocal: (text) async {
        sent.add(text);
        return accept;
      },
      takeRemote: () async {
        takes++;
        final text = remote;
        remote = null;
        return text;
      },
    );
  });

  test('disabled synchronization performs no clipboard IO', () async {
    local = 'private local text';
    remote = 'remote text';
    await sync.synchronize();
    expect([reads, takes], [0, 0]);
    expect(sent, isEmpty);
    expect(written, isEmpty);
  });

  test('sends each changed local text once, including empty text', () async {
    sync.enabled = true;
    local = 'first';
    await sync.synchronize();
    sync.enabled = true;
    await sync.synchronize();
    local = '';
    await sync.synchronize();
    await sync.synchronize();
    expect(sent, ['first', '']);
    expect(forcedReads, [true, false, false, false]);
  });

  test('writes remote text without reflecting it to its sender', () async {
    sync.enabled = true;
    local = 'old local';
    remote = 'remote';
    await sync.synchronize();
    remote = '';
    await sync.synchronize();
    expect(written, ['remote', '']);
    expect(sent, isEmpty);
    local = 'subsequent local copy';
    await sync.synchronize();
    expect(sent, ['subsequent local copy']);
  });

  test('forces a fresh read after native rejection', () async {
    sync.enabled = true;
    local = 'not accepted yet';
    accept = false;
    await sync.synchronize();
    accept = true;
    await sync.synchronize();
    await sync.synchronize();
    expect(sent, ['not accepted yet', 'not accepted yet']);
    expect(forcedReads, [true, true, false]);
  });

  test('does not overlap polls or apply remote text after disable', () async {
    final pending = Completer<String?>();
    sync = OhosClipboardSync(
      readLocal: (_) async => fail('must not read after cancellation'),
      writeLocal: (text) async => written.add(text),
      sendLocal: (_) async => fail('must not send after cancellation'),
      takeRemote: () {
        takes++;
        return pending.future;
      },
    );
    sync.enabled = true;
    final first = sync.synchronize();
    await sync.synchronize();
    expect(takes, 1);
    sync.enabled = false;
    pending.complete('stale remote');
    await first;
    expect(written, isEmpty);
  });

  test('does not send an old read across disable and reenable', () async {
    final pending = Completer<String?>();
    final enteredRead = Completer<void>();
    sync = OhosClipboardSync(
      readLocal: (force) {
        forcedReads.add(force);
        if (!enteredRead.isCompleted) {
          enteredRead.complete();
          return pending.future;
        }
        return Future.value('fresh text');
      },
      writeLocal: (_) async {},
      sendLocal: (text) async {
        sent.add(text);
        return true;
      },
      takeRemote: () async => null,
    );
    sync.enabled = true;
    final first = sync.synchronize();
    await enteredRead.future;
    sync.enabled = false;
    sync.enabled = true;
    pending.complete('old text');
    await first;
    expect(sent, isEmpty);
    await sync.synchronize();
    expect(sent, ['fresh text']);
    expect(forcedReads, [true, true]);
  });

  test('propagates current errors and releases the active poll', () async {
    var failRead = true;
    sync = OhosClipboardSync(
      readLocal: (_) async {
        if (failRead) throw StateError('read failed');
        return 'recovered';
      },
      writeLocal: (_) async {},
      sendLocal: (text) async {
        sent.add(text);
        return true;
      },
      takeRemote: () async => null,
    );
    sync.enabled = true;
    await expectLater(sync.synchronize(), throwsStateError);
    failRead = false;
    await sync.synchronize();
    expect(sent, ['recovered']);
  });

  test('an error from a cancelled poll cannot disable a new cycle', () async {
    final pending = Completer<String?>();
    sync = OhosClipboardSync(
      readLocal: (_) async => 'fresh',
      writeLocal: (_) async {},
      sendLocal: (text) async {
        sent.add(text);
        return true;
      },
      takeRemote: () {
        takes++;
        return takes == 1 ? pending.future : Future.value(null);
      },
    );
    sync.enabled = true;
    final first = sync.synchronize();
    sync.enabled = false;
    sync.enabled = true;
    pending.completeError(StateError('old request failed'));
    await first;
    await sync.synchronize();
    expect(sent, ['fresh']);
  });
}
