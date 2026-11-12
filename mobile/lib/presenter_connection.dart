import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';

/// Connection lifecycle as the UI presents it.
enum LinkState { disconnected, connecting, authenticating, ready, failed }

/// Speaks HyprPresenter protocol v1 (see the desktop README) to the overlay's
/// WebSocket server: JSON messages in, JSON envelope replies out, with the
/// pairing token as the first message on every connection.
///
/// Plain `dart:io` WebSocket on purpose — the protocol is tiny and needs no
/// extra dependency.  A 5 s client ping interval keeps the session alive
/// through the server's ping/pong handling.
class PresenterConnection extends ChangeNotifier {
  WebSocket? _socket;
  StreamSubscription<dynamic>? _sub;

  LinkState state = LinkState.disconnected;
  String? error;
  String? lastReply;
  String serverLabel = '';

  bool get isReady => state == LinkState.ready;
  bool get isBusy =>
      state == LinkState.connecting || state == LinkState.authenticating;

  /// Connect and authenticate.  Returns true when the session is ready for
  /// commands; on failure [state] is [LinkState.failed] and [error] says why.
  Future<bool> connect({
    required String host,
    required int port,
    required String token,
  }) async {
    await disconnect();
    state = LinkState.connecting;
    error = null;
    lastReply = null;
    serverLabel = 'ws://$host:$port';
    notifyListeners();
    try {
      final socket = await WebSocket.connect(serverLabel)
          .timeout(const Duration(seconds: 8));
      _socket = socket;
      socket.pingInterval = const Duration(seconds: 5);
      state = LinkState.authenticating;
      notifyListeners();

      final authResult = Completer<bool>();
      _sub = socket.listen(
        (data) => _onMessage(data, authResult),
        onError: (Object e) {
          if (!authResult.isCompleted) authResult.complete(false);
          _fail('connection error: $e');
        },
        onDone: () {
          if (!authResult.isCompleted) authResult.complete(false);
          _fail('connection closed');
        },
      );
      _sendRaw({'v': 1, 'type': 'auth', 'token': token});
      final ok = await authResult.future.timeout(
        const Duration(seconds: 8),
        onTimeout: () => false,
      );
      if (ok) {
        state = LinkState.ready;
        notifyListeners();
      }
      return ok;
    } catch (e) {
      _fail('cannot reach $serverLabel — $e');
      return false;
    }
  }

  Future<void> disconnect() async {
    await _sub?.cancel();
    _sub = null;
    try {
      await _socket?.close();
    } catch (_) {
      // Already gone; disconnect must never throw into the UI.
    }
    _socket = null;
    if (state != LinkState.disconnected) {
      state = LinkState.disconnected;
      notifyListeners();
    }
  }

  // -- protocol v1 commands -------------------------------------------------

  void sendCommand(Map<String, dynamic> fields) {
    if (!isReady) return;
    _sendRaw({'v': 1, ...fields});
  }

  /// Fire-and-forget for commands the server answers: the reply is
  /// still expected (and consumed, in order) — it is just not awaited
  /// here.  Registering every answered command keeps the reply FIFO
  /// aligned: a reply can never land in the wrong caller's hands.
  void _fire(Map<String, dynamic> fields) {
    requestReply(fields).ignore();
  }

  void next() => _fire({'cmd': 'next'});
  void previous() => _fire({'cmd': 'previous'});
  void center() => _fire({'cmd': 'center'});
  void show() => _fire({'cmd': 'show'});
  void hide() => _fire({'cmd': 'hide'});
  void status() => _fire({'cmd': 'status'});

  /// Laser moves are the one unanswered command: they stream at ~50/s
  /// and the server applies them without a reply (see net.rs), so no
  /// reply slot is registered for them either.
  void moveTo(int x, int y) => sendCommand({'cmd': 'move', 'x': x, 'y': y});

  final List<Completer<String>> _pendingReplies = [];

  /// Send a command and await its reply text.  The server answers every
  /// command except streaming laser moves exactly once, in order, and
  /// every answered send registers here, so a FIFO of completers matches
  /// replies to requests.
  Future<String> requestReply(Map<String, dynamic> fields) {
    if (!isReady) {
      return Future.value('error: not connected');
    }
    final completer = Completer<String>();
    _pendingReplies.add(completer);
    _sendRaw({'v': 1, ...fields});
    return completer.future.timeout(
      const Duration(seconds: 3),
      onTimeout: () {
        _pendingReplies.remove(completer);
        return 'error: reply timeout';
      },
    );
  }

  /// Await the full status line (outputs, confinement, dot position).
  Future<String> requestStatus() => requestReply({'cmd': 'status'});

  /// Confine the laser to one output, or release it with null.
  Future<String> confineTo(String? outputName) =>
      requestReply({'cmd': 'confine', 'output': outputName});

  /// Fullscreen the desktop's active window (the presentation).
  /// [on] null toggles; the reply reports the resulting state.
  Future<String> fullscreen({bool? on}) =>
      requestReply({'cmd': 'fullscreen', 'on': ?on});

  // -- internals --------------------------------------------------------------

  void _sendRaw(Map<String, dynamic> message) {
    try {
      _socket?.add(jsonEncode(message));
    } catch (e) {
      // A dead socket must not throw into gesture handlers (the laser
      // button sends from a pointer callback): flip the state to failed
      // so the UI stops trusting the link and the user can reconnect.
      if (state == LinkState.ready || state == LinkState.authenticating) {
        _fail('connection lost: $e');
      }
    }
  }

  void _onMessage(dynamic data, Completer<bool> authResult) {
    if (data is! String) return;
    Map<String, dynamic> reply;
    try {
      reply = jsonDecode(data) as Map<String, dynamic>;
    } catch (_) {
      return; // Not a protocol reply; nothing the UI can do with it.
    }
    final ok = reply['ok'] == true;
    if (state == LinkState.authenticating) {
      if (ok) {
        lastReply = 'authenticated';
        if (!authResult.isCompleted) authResult.complete(true);
      } else {
        error = reply['error']?.toString() ?? 'authentication failed';
        if (!authResult.isCompleted) authResult.complete(false);
        _fail(error!);
      }
      return;
    }
    lastReply = ok
        ? reply['reply']?.toString()
        : 'error: ${reply['error'] ?? 'unknown'}';
    if (_pendingReplies.isNotEmpty) {
      _pendingReplies.removeAt(0).complete(lastReply!);
    }
    notifyListeners();
  }

  void _fail(String message) {
    error = message;
    state = LinkState.failed;
    for (final completer in _pendingReplies) {
      if (!completer.isCompleted) completer.complete('error: $message');
    }
    _pendingReplies.clear();
    final socket = _socket;
    _socket = null;
    _sub?.cancel();
    _sub = null;
    socket?.close().ignore();
    notifyListeners();
  }

  @override
  void dispose() {
    _sub?.cancel();
    _socket?.close().ignore();
    super.dispose();
  }
}
