class OhosClipboardSync {
  OhosClipboardSync({
    required this.readLocal,
    required this.writeLocal,
    required this.sendLocal,
    required this.takeRemote,
  });

  final Future<String?> Function(bool force) readLocal;
  final Future<void> Function(String text) writeLocal;
  final Future<bool> Function(String text) sendLocal;
  final Future<String?> Function() takeRemote;

  bool _enabled = false;
  bool _busy = false;
  bool _forceRead = true;
  int _generation = 0;
  String? _lastText;

  bool get enabled => _enabled;

  set enabled(bool value) {
    if (_enabled == value) return;
    _enabled = value;
    _generation++;
    _forceRead = true;
    _lastText = null;
  }

  bool _isCurrent(int generation) => _enabled && generation == _generation;

  Future<void> synchronize() async {
    if (!_enabled || _busy) return;
    _busy = true;
    final generation = _generation;
    try {
      final remote = await takeRemote();
      if (!_isCurrent(generation)) return;
      if (remote != null) {
        await writeLocal(remote);
        if (!_isCurrent(generation)) return;
        _lastText = remote;
      }

      final local = await readLocal(_forceRead);
      if (!_isCurrent(generation)) return;
      _forceRead = false;
      if (local == null || local == _lastText) return;
      final accepted = await sendLocal(local);
      if (!_isCurrent(generation)) return;
      if (accepted) {
        _lastText = local;
      } else {
        // The platform may have cached this revision even though Rust rejected it.
        _forceRead = true;
      }
    } catch (_) {
      if (_isCurrent(generation)) rethrow;
    } finally {
      _busy = false;
    }
  }
}
