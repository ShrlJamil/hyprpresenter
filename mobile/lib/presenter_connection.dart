import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:math' as math;

import 'package:flutter/foundation.dart';

/// Connection lifecycle as the UI presents it.
enum LinkState {
  disconnected,
  connecting,
  authenticating,
  ready,
  failed,
  reconnecting,
}

/// Speaks HyprPresenter protocol v1 (see the desktop README) to the overlay's
/// WebSocket server: JSON messages in, JSON envelope replies out, with the
/// pairing token as the first message on every connection.
///
/// Plain `dart:io` WebSocket on purpose — the protocol is tiny and needs no
/// extra dependency.  A 5 s client ping interval keeps the session alive
/// through the server's ping/pong handling.
///
/// A dropped link reconnects automatically: while the user has not asked
/// to disconnect, transport failures retry with exponential backoff
/// (1 s, doubling to a 30 s cap) until the server answers again.  Only an
/// authentication rejection stops the retries — re-pairing rotates the
/// token, so a rejected one will never succeed and the user must re-pair.
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
  bool get isReconnecting => state == LinkState.reconnecting;

  // -- auto-reconnect bookkeeping --------------------------------------------

  String? _host;
  int? _port;
  String? _token;

  /// The user's standing intent to be linked: set by [connect], cleared
  /// only by an explicit [disconnect].  Transport failures retry while
  /// this is set; a manual disconnect is never second-guessed.
  bool _wantConnected = false;

  /// Bumped by every [connect]/[disconnect], so an attempt that finishes
  /// late (its socket finally opened after the user moved on) can tell
  /// it has been superseded and quietly close instead of resurrecting a
  /// session nobody wants anymore.
  int _gen = 0;

  /// True between sending the auth message and handling its reply.
  /// Tracked separately from [state] because a reconnect attempt keeps
  /// [state] at [LinkState.reconnecting] through its auth phase too.
  bool _authPending = false;

  Timer? _retryTimer;
  int _retryAttempt = 0;

  /// How many reconnect attempts have been scheduled since the drop
  /// (0 while the link is healthy); the UI shows it as progress.
  int get reconnectAttempt => _retryAttempt;

  /// Connect and authenticate.  Returns true when the session is ready
  /// for commands.  On a transport failure it returns false and the
  /// state becomes [LinkState.reconnecting] while the backoff loop keeps
  /// trying in the background — a later success flips the state to
  /// [LinkState.ready], which listeners can act on.  Only an
  /// authentication rejection lands in [LinkState.failed] for good.
  Future<bool> connect({
    required String host,
    required int port,
    required String token,
  }) async {
    _host = host;
    _port = port;
    _token = token;
    _wantConnected = true;
    _retryTimer?.cancel();
    _retryTimer = null;
    _retryAttempt = 0;
    final gen = ++_gen;
    return _attempt(gen);
  }

  /// One connect + authenticate round against the stored target.
  Future<bool> _attempt(int gen) async {
    await _closeSocket();
    if (gen != _gen) return false;
    state = _retryAttempt > 0 ? LinkState.reconnecting : LinkState.connecting;
    error = null;
    lastReply = null;
    serverLabel = 'ws://$_host:$_port';
    notifyListeners();
    try {
      final socket = await WebSocket.connect(serverLabel)
          .timeout(const Duration(seconds: 8));
      if (gen != _gen) {
        socket.close().ignore();
        return false;
      }
      _socket = socket;
      socket.pingInterval = const Duration(seconds: 5);
      if (_retryAttempt == 0) {
        state = LinkState.authenticating;
        notifyListeners();
      }

      final authResult = Completer<bool>();
      _sub = socket.listen(
        (data) => _onMessage(data, authResult),
        onError: (Object e) {
          if (!authResult.isCompleted) authResult.complete(false);
          if (gen == _gen) _fail('connection error: $e');
        },
        onDone: () {
          if (!authResult.isCompleted) authResult.complete(false);
          if (gen == _gen) _fail('connection closed');
        },
      );
      _authPending = true;
      _sendRaw({'v': 1, 'type': 'auth', 'token': _token});
      final ok = await authResult.future.timeout(
        const Duration(seconds: 8),
        onTimeout: () => false,
      );
      if (gen != _gen) return false;
      if (ok) {
        _retryAttempt = 0;
        state = LinkState.ready;
        notifyListeners();
      } else if (_authPending) {
        // Nobody failed this attempt on our behalf (an auth rejection
        // or a socket error would have): the server never answered.
        _authPending = false;
        _fail('authentication timed out');
      }
      return ok;
    } catch (e) {
      if (gen == _gen) _fail('cannot reach $serverLabel — $e');
      return false;
    }
  }

  Future<void> disconnect() async {
    _wantConnected = false;
    _gen++;
    _retryTimer?.cancel();
    _retryTimer = null;
    _retryAttempt = 0;
    await _closeSocket();
    if (state != LinkState.disconnected) {
      state = LinkState.disconnected;
      notifyListeners();
    }
  }

  Future<void> _closeSocket() async {
    _authPending = false;
    await _sub?.cancel();
    _sub = null;
    try {
      await _socket?.close();
    } catch (_) {
      // Already gone; closing must never throw into the UI.
    }
    _socket = null;
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
      // button sends from a pointer callback): fail the link so the UI
      // stops trusting it and the auto-reconnect backoff takes over.
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
    if (_authPending) {
      _authPending = false;
      if (ok) {
        lastReply = 'authenticated';
        if (!authResult.isCompleted) authResult.complete(true);
      } else {
        error = reply['error']?.toString() ?? 'authentication failed';
        if (!authResult.isCompleted) authResult.complete(false);
        // A rejected token will never start working again (re-pairing
        // rotates it), so this failure is terminal: no auto-reconnect.
        _fail(error!, retryable: false);
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

  void _fail(String message, {bool retryable = true}) {
    error = message;
    _authPending = false;
    for (final completer in _pendingReplies) {
      if (!completer.isCompleted) completer.complete('error: $message');
    }
    _pendingReplies.clear();
    final socket = _socket;
    _socket = null;
    _sub?.cancel();
    _sub = null;
    socket?.close().ignore();
    if (retryable && _wantConnected) {
      state = LinkState.reconnecting;
      _scheduleRetry();
    } else {
      state = LinkState.failed;
    }
    notifyListeners();
  }

  /// Exponential backoff between reconnect attempts: 1 s, 2 s, 4 s,
  /// 8 s, 16 s, then a 30 s cap for as long as the user still wants
  /// the link.  The server takes one client at a time and queues new
  /// connections in its TCP backlog, so a slow cadence is never what
  /// keeps a recovering session waiting.
  void _scheduleRetry() {
    _retryTimer?.cancel();
    _retryAttempt++;
    final delay = Duration(
      seconds: math.min(30, 1 << math.min(_retryAttempt - 1, 5)),
    );
    final gen = _gen;
    _retryTimer = Timer(delay, () {
      _retryTimer = null;
      if (gen != _gen || !_wantConnected) return;
      _attempt(gen);
    });
  }

  @override
  void dispose() {
    _wantConnected = false;
    _gen++;
    _retryTimer?.cancel();
    _sub?.cancel();
    _socket?.close().ignore();
    super.dispose();
  }
}
