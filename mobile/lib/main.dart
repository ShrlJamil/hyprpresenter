import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:sensors_plus/sensors_plus.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'desktop_status.dart';
import 'gyro_pointer.dart';
import 'pairing.dart';
import 'presenter_connection.dart';
import 'qr_scan_screen.dart';

/// HyprPresenter mobile remote (Phase 6).
///
/// Pairing is QR-only by design: the desktop shows a QR via
/// `hyprpresenter pair`, and this app's start screen is a single scan
/// button.  Once paired, the values persist and later launches reconnect
/// automatically.  The clicker screen carries slide navigation and the
/// dominant hold-to-activate gyroscope laser button.
void main() {
  runApp(const HyprPresenterApp());
}

class HyprPresenterApp extends StatelessWidget {
  const HyprPresenterApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'HyprPresenter',
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(
          seedColor: Colors.red,
          brightness: Brightness.dark,
        ),
        useMaterial3: true,
      ),
      home: const PairingGate(),
    );
  }
}

// ---------------------------------------------------------------------------
// Start gate: QR-only pairing, deliberately minimal.
// ---------------------------------------------------------------------------

enum _GatePhase { loading, needsPairing, connecting, failed }

class PairingGate extends StatefulWidget {
  const PairingGate({super.key, this.autoConnect = true});

  /// False when returning here from a manual disconnect: show the stored
  /// pairing as a button instead of reconnecting on sight.
  final bool autoConnect;

  @override
  State<PairingGate> createState() => _PairingGateState();
}

class _PairingGateState extends State<PairingGate> {
  final PresenterConnection _conn = PresenterConnection();
  _GatePhase _phase = _GatePhase.loading;
  PairingValues? _stored;
  String? _error;

  @override
  void initState() {
    super.initState();
    _boot();
  }

  Future<void> _boot() async {
    final prefs = await SharedPreferences.getInstance();
    final host = prefs.getString('pair_host');
    final port = prefs.getInt('pair_port');
    final token = prefs.getString('pair_token');
    if (!mounted) return;
    if (host == null || port == null || token == null) {
      setState(() => _phase = _GatePhase.needsPairing);
      return;
    }
    _stored = (host: host, port: port, token: token);
    if (widget.autoConnect) {
      _connect(_stored!);
    } else {
      setState(() => _phase = _GatePhase.needsPairing);
    }
  }

  Future<void> _connect(PairingValues values) async {
    setState(() {
      _phase = _GatePhase.connecting;
      _error = null;
    });
    final ok = await _conn.connect(
      host: values.host,
      port: values.port,
      token: values.token,
    );
    if (!mounted) return;
    if (ok) {
      await Navigator.of(context).pushReplacement(
        MaterialPageRoute(builder: (_) => ClickerPage(connection: _conn)),
      );
      if (mounted) _boot();
    } else {
      setState(() {
        _phase = _GatePhase.failed;
        _error = _conn.error;
      });
    }
  }

  Future<void> _scan() async {
    final values = await Navigator.of(context).push<PairingValues>(
      MaterialPageRoute(builder: (_) => const QrScanScreen()),
    );
    if (values == null || !mounted) return;
    final prefs = await SharedPreferences.getInstance();
    await prefs.setString('pair_host', values.host);
    await prefs.setInt('pair_port', values.port);
    await prefs.setString('pair_token', values.token);
    _stored = values;
    _connect(values);
  }

  @override
  void dispose() {
    _conn.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      body: SafeArea(
        child: Center(
          child: Padding(
            padding: const EdgeInsets.all(32),
            child: switch (_phase) {
              _GatePhase.loading || _GatePhase.connecting => const Column(
                mainAxisSize: MainAxisSize.min,
                children: [
                  CircularProgressIndicator(),
                  SizedBox(height: 16),
                  Text('Menghubungkan…'),
                ],
              ),
              _GatePhase.needsPairing || _GatePhase.failed => Column(
                mainAxisSize: MainAxisSize.min,
                children: [
                  if (_phase == _GatePhase.failed && _error != null) ...[
                    Text(
                      _error!,
                      textAlign: TextAlign.center,
                      style: TextStyle(
                        color: Theme.of(context).colorScheme.error,
                      ),
                    ),
                    const SizedBox(height: 20),
                  ],
                  if (_stored != null && !widget.autoConnect) ...[
                    SizedBox(
                      width: double.infinity,
                      height: 60,
                      child: FilledButton.icon(
                        onPressed: () => _connect(_stored!),
                        icon: const Icon(Icons.link),
                        label: Text(
                          'Hubungkan ke ${_stored!.host}:${_stored!.port}',
                        ),
                      ),
                    ),
                    const SizedBox(height: 12),
                  ],
                  SizedBox(
                    width: double.infinity,
                    height: 60,
                    child: FilledButton.icon(
                      onPressed: _scan,
                      icon: const Icon(Icons.qr_code_scanner, size: 28),
                      label: const Text(
                        'Scan QR',
                        style: TextStyle(fontSize: 18),
                      ),
                    ),
                  ),
                  if (_phase == _GatePhase.failed && _stored != null) ...[
                    const SizedBox(height: 8),
                    TextButton(
                      onPressed: () => _connect(_stored!),
                      child: const Text('Coba lagi'),
                    ),
                  ],
                ],
              ),
            },
          ),
        ),
      ),
    );
  }
}

// ---------------------------------------------------------------------------
// Clicker: slide navigation + dominant hold-to-laser button.
// ---------------------------------------------------------------------------

class ClickerPage extends StatefulWidget {
  const ClickerPage({super.key, required this.connection});

  final PresenterConnection connection;

  @override
  State<ClickerPage> createState() => _ClickerPageState();
}

class _ClickerPageState extends State<ClickerPage> {
  PresenterConnection get _conn => widget.connection;

  DesktopStatus? _status;
  String? _confined;
  bool _autoConfinedOnce = false;

  final GyroPointer _gyro = GyroPointer();
  StreamSubscription<GyroscopeEvent>? _gyroSub;
  StreamSubscription<AccelerometerEvent>? _accelSub;

  bool _laserHeld = false;
  Timer? _laserTimer;
  double _smoothX = 0;
  double _smoothY = 0;
  int _lastSentX = -1;
  int _lastSentY = -1;

  /// Direction calibration for the gyro mapping.  If a direction feels
  /// inverted on the real device, flip the matching sign here — nothing
  /// else depends on these.
  static const double _yawSign = -1;
  static const double _pitchSign = -1;

  /// Radians of tilt that sweep the full width/height of the active area
  /// (≈ 63°): reachable with wrist movement alone, no body turning.
  static const double _sweepRadians = 1.1;

  static const double _deadzone = 0.008;

  @override
  void initState() {
    super.initState();
    _gyroSub = gyroscopeEventStream(samplingPeriod: SensorInterval.gameInterval)
        .listen(
          (e) => _gyro.addGyroscope(e.x, e.y, e.z, DateTime.now()),
          onError: (_) {}, // No sensor (emulator): laser simply stays centred.
        );
    _accelSub = accelerometerEventStream(
      samplingPeriod: SensorInterval.gameInterval,
    ).listen((e) => _gyro.addAccelerometer(e.x, e.y, e.z), onError: (_) {});
    _refreshStatus();
  }

  @override
  void dispose() {
    _laserTimer?.cancel();
    _gyroSub?.cancel();
    _accelSub?.cancel();
    super.dispose();
  }

  Future<void> _refreshStatus() async {
    final reply = await _conn.requestStatus();
    final parsed = parseDesktopStatus(reply);
    if (parsed == null || !mounted) return;
    setState(() {
      _status = parsed;
      _confined = parsed.confined;
    });
    // Default on multi-monitor setups: lock the laser to the monitor it
    // is currently on, so it cannot wander onto the other screen.
    if (!_autoConfinedOnce &&
        parsed.outputs.length > 1 &&
        parsed.confined == null &&
        parsed.onOutput != null) {
      _autoConfinedOnce = true;
      await _pickConfine(parsed.onOutput);
    }
  }

  Future<void> _pickConfine(String? name) async {
    final reply = await _conn.confineTo(name);
    if (!mounted) return;
    if (reply.startsWith('ok')) {
      setState(() => _confined = name);
    }
  }

  /// The rectangle the gyro steers within: the confined output, else the
  /// union of all outputs.
  ({int x, int y, int w, int h}) get _activeRect {
    final status = _status;
    if (status == null) {
      return (x: 0, y: 0, w: 1920, h: 1080);
    }
    final confined = status.outputNamed(_confined);
    if (confined != null) {
      return (
        x: confined.x,
        y: confined.y,
        w: confined.width,
        h: confined.height,
      );
    }
    final b = status.bounds;
    return (x: b.minX, y: b.minY, w: b.maxX - b.minX, h: b.maxY - b.minY);
  }

  void _laserDown() {
    HapticFeedback.mediumImpact();
    _gyro.anchor();
    final rect = _activeRect;
    _smoothX = rect.x + rect.w / 2;
    _smoothY = rect.y + rect.h / 2;
    _lastSentX = -1;
    _lastSentY = -1;
    _conn.center();
    _conn.show();
    setState(() => _laserHeld = true);
    _laserTimer?.cancel();
    _laserTimer = Timer.periodic(
      const Duration(milliseconds: 33),
      (_) => _laserTick(),
    );
  }

  void _laserTick() {
    final dev = _gyro.deviation;
    if (dev == null) return;
    final rect = _activeRect;
    final centerX = rect.x + rect.w / 2;
    final centerY = rect.y + rect.h / 2;
    var yaw = dev.yaw;
    var pitch = dev.pitch;
    if (yaw.abs() < _deadzone) yaw = 0;
    if (pitch.abs() < _deadzone) pitch = 0;
    final targetX = centerX + _yawSign * yaw * (rect.w / _sweepRadians);
    final targetY = centerY + _pitchSign * pitch * (rect.h / _sweepRadians);
    _smoothX += (targetX - _smoothX) * 0.45;
    _smoothY += (targetY - _smoothY) * 0.45;
    final sendX = _smoothX.round().clamp(rect.x, rect.x + rect.w - 1);
    final sendY = _smoothY.round().clamp(rect.y, rect.y + rect.h - 1);
    if ((sendX - _lastSentX).abs() >= 2 || (sendY - _lastSentY).abs() >= 2) {
      _lastSentX = sendX;
      _lastSentY = sendY;
      _conn.moveTo(sendX, sendY);
    }
  }

  void _laserUp() {
    if (!_laserHeld) return;
    _laserTimer?.cancel();
    _laserTimer = null;
    _conn.hide();
    setState(() => _laserHeld = false);
  }

  Future<void> _disconnect() async {
    _laserUp();
    await _conn.disconnect();
    if (!mounted) return;
    await Navigator.of(context).pushReplacement(
      MaterialPageRoute(builder: (_) => const PairingGate(autoConnect: false)),
    );
  }

  @override
  Widget build(BuildContext context) {
    final outputs = _status?.outputs ?? const <OutputInfo>[];
    return Scaffold(
      appBar: AppBar(
        title: const Text('HyprPresenter'),
        actions: [
          IconButton(
            icon: const Icon(Icons.link_off),
            tooltip: 'Putuskan',
            onPressed: _disconnect,
          ),
        ],
      ),
      body: SafeArea(
        child: Column(
          children: [
            AnimatedBuilder(
              animation: _conn,
              builder: (context, _) {
                final reply = _conn.lastReply ?? '';
                if (!reply.startsWith('error')) {
                  return const SizedBox.shrink();
                }
                return Padding(
                  padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
                  child: Text(
                    reply,
                    style: TextStyle(
                      color: Theme.of(context).colorScheme.error,
                    ),
                  ),
                );
              },
            ),
            if (outputs.length > 1)
              Padding(
                padding: const EdgeInsets.fromLTRB(16, 12, 16, 0),
                child: Column(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    const Text(
                      'Monitor laser',
                      style: TextStyle(fontWeight: FontWeight.w600),
                    ),
                    const SizedBox(height: 6),
                    Wrap(
                      spacing: 8,
                      children: [
                        ChoiceChip(
                          label: const Text('Semua'),
                          selected: _confined == null,
                          onSelected: (_) => _pickConfine(null),
                        ),
                        for (final o in outputs)
                          ChoiceChip(
                            label: Text(o.name),
                            selected: _confined == o.name,
                            onSelected: (_) => _pickConfine(o.name),
                          ),
                      ],
                    ),
                  ],
                ),
              ),
            Padding(
              padding: const EdgeInsets.all(16),
              child: Row(
                children: [
                  Expanded(
                    child: SizedBox(
                      height: 68,
                      child: OutlinedButton.icon(
                        onPressed: _conn.previous,
                        icon: const Icon(Icons.arrow_back),
                        label: const Text('Sebelumnya'),
                      ),
                    ),
                  ),
                  const SizedBox(width: 12),
                  Expanded(
                    child: SizedBox(
                      height: 68,
                      child: FilledButton.icon(
                        onPressed: _conn.next,
                        icon: const Icon(Icons.arrow_forward),
                        label: const Text('Berikutnya'),
                      ),
                    ),
                  ),
                ],
              ),
            ),
            Expanded(
              child: Padding(
                padding: const EdgeInsets.fromLTRB(16, 0, 16, 16),
                child: Listener(
                  behavior: HitTestBehavior.opaque,
                  onPointerDown: (_) => _laserDown(),
                  onPointerUp: (_) => _laserUp(),
                  onPointerCancel: (_) => _laserUp(),
                  child: Container(
                    decoration: BoxDecoration(
                      color: _laserHeld
                          ? Colors.red.shade600
                          : Colors.red.shade900,
                      borderRadius: BorderRadius.circular(28),
                    ),
                    child: Center(
                      child: Column(
                        mainAxisSize: MainAxisSize.min,
                        children: [
                          Icon(
                            _laserHeld
                                ? Icons.radio_button_checked
                                : Icons.highlight,
                            size: 72,
                            color: Colors.white,
                          ),
                          const SizedBox(height: 12),
                          const Text(
                            'LASER',
                            style: TextStyle(
                              color: Colors.white,
                              fontSize: 28,
                              fontWeight: FontWeight.bold,
                              letterSpacing: 4,
                            ),
                          ),
                          const SizedBox(height: 6),
                          Text(
                            _laserHeld
                                ? 'Gerakkan HP untuk mengarahkan'
                                : 'Tahan untuk menyalakan',
                            style: const TextStyle(color: Colors.white70),
                          ),
                        ],
                      ),
                    ),
                  ),
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }
}
