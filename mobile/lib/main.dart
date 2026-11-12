import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_svg/flutter_svg.dart';
import 'package:package_info_plus/package_info_plus.dart';
import 'package:sensors_plus/sensors_plus.dart';
import 'package:shared_preferences/shared_preferences.dart';

import 'desktop_status.dart';
import 'notes.dart';
import 'notes_editor.dart';
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
  WidgetsFlutterBinding.ensureInitialized();
  SystemChrome.setPreferredOrientations(const [DeviceOrientation.portraitUp]);
  runApp(const HyprPresenterApp());
}

class HyprPresenterApp extends StatefulWidget {
  const HyprPresenterApp({super.key});

  @override
  State<HyprPresenterApp> createState() => _HyprPresenterAppState();
}

class _HyprPresenterAppState extends State<HyprPresenterApp> {
  /// The one connection for the app's lifetime.  It is owned HERE, not by
  /// the pairing gate: the gate hands the live connection to the clicker
  /// via navigation, and a route that gets replaced must never dispose a
  /// connection another screen is using (that exact bug closed the socket
  /// under the clicker in the first 6.2 build).
  late final PresenterConnection _connection = PresenterConnection();

  @override
  void dispose() {
    _connection.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'HyprPresenter',
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(
          seedColor: Colors.green,
          brightness: Brightness.dark,
        ),
        useMaterial3: true,
      ),
      home: PairingGate(connection: _connection),
    );
  }
}

// ---------------------------------------------------------------------------
// Start gate: QR-only pairing, deliberately minimal.
// ---------------------------------------------------------------------------

enum _GatePhase { loading, needsPairing, connecting, failed }

class PairingGate extends StatefulWidget {
  const PairingGate({
    super.key,
    required this.connection,
    this.autoConnect = true,
  });

  /// The app-lifetime connection, owned by the root widget.
  final PresenterConnection connection;

  /// False when returning here from a manual disconnect: show the stored
  /// pairing as a button instead of reconnecting on sight.
  final bool autoConnect;

  @override
  State<PairingGate> createState() => _PairingGateState();
}

class _PairingGateState extends State<PairingGate> {
  PresenterConnection get _conn => widget.connection;
  _GatePhase _phase = _GatePhase.loading;

  /// This build's version ("0.1.0") from the package metadata — the
  /// value the planned update checker compares against the latest
  /// GitHub release tag.
  String? _version;
  PairingValues? _stored;
  String? _error;

  @override
  void initState() {
    super.initState();
    _loadVersion();
    _boot();
  }

  Future<void> _loadVersion() async {
    final info = await PackageInfo.fromPlatform();
    if (mounted) setState(() => _version = info.version);
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

  Future<void> _openNotesEditor() async {
    await Navigator.of(context)
        .push(MaterialPageRoute(builder: (_) => const NotesEditorPage()));
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      body: SafeArea(
        child: Column(
          children: [
            Padding(
              padding: const EdgeInsets.only(top: 64, bottom: 8),
              child: SvgPicture.asset('assets/logo.svg', height: 118),
            ),
            Expanded(
              child: Center(
                child: Padding(
                  padding: const EdgeInsets.all(32),
                  child: _phaseBody(),
                ),
              ),
            ),
            Padding(
              padding: const EdgeInsets.only(bottom: 6),
              child: IconButton.filledTonal(
                icon: const Icon(Icons.sticky_note_2_outlined),
                iconSize: 28,
                tooltip: 'Slide notes',
                onPressed: _openNotesEditor,
              ),
            ),
            Padding(
              padding: const EdgeInsets.only(bottom: 14),
              child: Text(
                _version == null ? '' : 'v$_version',
                style: TextStyle(
                  fontSize: 11,
                  color: Theme.of(context).colorScheme.onSurfaceVariant,
                ),
              ),
            ),
          ],
        ),
      ),
    );
  }

  Widget _phaseBody() {
    return switch (_phase) {
      _GatePhase.loading || _GatePhase.connecting => const Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          CircularProgressIndicator(),
          SizedBox(height: 16),
          Text('Connecting…'),
        ],
      ),
      _GatePhase.needsPairing || _GatePhase.failed => Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          if (_phase == _GatePhase.failed && _error != null) ...[
            Text(
              _error!,
              textAlign: TextAlign.center,
              style: TextStyle(color: Theme.of(context).colorScheme.error),
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
                label: Text('Connect to ${_stored!.host}:${_stored!.port}'),
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
              label: const Text('Scan QR', style: TextStyle(fontSize: 18)),
            ),
          ),
          if (_phase == _GatePhase.failed && _stored != null) ...[
            const SizedBox(height: 8),
            TextButton(
              onPressed: () => _connect(_stored!),
              child: const Text('Try again'),
            ),
          ],
        ],
      ),
    };
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

  List<String> _notes = <String>[];
  int _slide = 1;

  /// Whether we believe the desktop's active window is fullscreen;
  /// drives the toggle icon at the top-left of the clicker.  Flipped
  /// optimistically on each tap (the server performs the toggle).
  bool _fullscreen = false;

  void _toggleFullscreen() {
    // The fullscreen target is the DESKTOP's active window (the
    // presentation), not this app: the icon flips optimistically and
    // the server toggles the window through Hyprland IPC.
    setState(() => _fullscreen = !_fullscreen);
    _conn.fullscreen().ignore();
  }

  final GyroPointer _gyro = GyroPointer();
  StreamSubscription<GyroscopeEvent>? _gyroSub;
  StreamSubscription<AccelerometerEvent>? _accelSub;

  bool _laserHeld = false;
  Timer? _laserTimer;
  double _smoothX = 0;
  double _smoothY = 0;
  int _lastSentX = -1;
  int _lastSentY = -1;

  /// Direction calibration for the gyro mapping (barrel-deflection
  /// model): leaning the top edge right gives positive yaw, nodding it
  /// toward you gives positive pitch; these signs turn both into
  /// natural dot movement.  Tuned on the real device (2026-11-11).
  static const double _yawSign = 1;
  static const double _pitchSign = -1;

  /// Radians of tilt that sweep the full width/height of the active
  /// area (0.67 rad ≈ 38° total, ±19° from centre), tuned on-device.
  static const double _sweepRadians = 0.67;

  static const double _deadzone = 0.008;

  @override
  void initState() {
    super.initState();
    _gyroSub = gyroscopeEventStream(samplingPeriod: SensorInterval.gameInterval)
        .listen(
          (e) => _gyro.addGyroscope(e.x, e.y, e.z, e.timestamp),
          onError: (_) {}, // No sensor (emulator): laser simply stays centred.
        );
    _accelSub = accelerometerEventStream(
      samplingPeriod: SensorInterval.gameInterval,
    ).listen((e) => _gyro.addAccelerometer(e.x, e.y, e.z), onError: (_) {});
    _refreshStatus();
    _loadNotes();
    // The overlay powers on with its dot visible; the product rule is
    // "laser only while held", so park it as soon as the clicker opens.
    _conn.hide();
  }

  @override
  void dispose() {
    _laserTimer?.cancel();
    _gyroSub?.cancel();
    _accelSub?.cancel();
    super.dispose();
  }

  Future<void> _loadNotes() async {
    final notes = await NotesStore.load();
    if (mounted) setState(() => _notes = notes);
  }

  Future<void> _openNotesEditor() async {
    await Navigator.of(context)
        .push(MaterialPageRoute(builder: (_) => const NotesEditorPage()));
    await _loadNotes();
  }

  Future<void> _openNotesFile() async {
    final notes = await pickNotesFile();
    if (notes == null) return;
    await NotesStore.save(notes);
    if (mounted) {
      setState(() {
        _notes = notes;
        _slide = 1;
      });
    }
  }

  void _goNext() {
    _conn.next();
    setState(() => _slide++);
  }

  void _goPrevious() {
    _conn.previous();
    if (_slide > 1) setState(() => _slide--);
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
      const Duration(milliseconds: 20),
      (_) => _laserTick(),
    );
  }

  void _laserTick() {
    if (!_conn.isReady) {
      _laserUp();
      return;
    }
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
    _smoothX += (targetX - _smoothX) * 0.5;
    _smoothY += (targetY - _smoothY) * 0.5;
    final sendX = _smoothX.round().clamp(rect.x, rect.x + rect.w - 1);
    final sendY = _smoothY.round().clamp(rect.y, rect.y + rect.h - 1);
    if (sendX != _lastSentX || sendY != _lastSentY) {
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
      MaterialPageRoute(
        builder: (_) => PairingGate(connection: _conn, autoConnect: false),
      ),
    );
  }

  /// The slide-notes area: deliberately tall, so the Previous/Next
  /// row sits closer to the middle of the screen.  Header carries the
  /// "Slide notes" caption and — when notes exist — the slide counter
  /// with re-sync chevrons; the action buttons (create / open file, or
  /// edit) live in the bottom-right corner.
  Widget _notesBox(ColorScheme scheme) {
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 12, 16, 0),
      child: Container(
        height: 200,
        width: double.infinity,
        decoration: BoxDecoration(
          color: scheme.surfaceContainerHighest,
          borderRadius: BorderRadius.circular(20),
        ),
        child: Padding(
          padding: const EdgeInsets.fromLTRB(16, 12, 12, 12),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Icon(
                    Icons.sticky_note_2_outlined,
                    size: 17,
                    color: scheme.onSurfaceVariant,
                  ),
                  const SizedBox(width: 8),
                  Text(
                    'Slide notes',
                    style: TextStyle(
                      color: scheme.onSurfaceVariant,
                      fontWeight: FontWeight.w600,
                    ),
                  ),
                  const Spacer(),
                  if (_notes.isNotEmpty) ...[
                    Text(
                      '$_slide / ${_notes.length}',
                      style: TextStyle(
                        color: scheme.onSurfaceVariant,
                        fontWeight: FontWeight.w600,
                        fontSize: 13,
                      ),
                    ),
                    IconButton(
                      icon: const Icon(Icons.chevron_left, size: 20),
                      visualDensity: VisualDensity.compact,
                      tooltip: 'Previous slide note',
                      onPressed: _slide > 1
                          ? () => setState(() => _slide--)
                          : null,
                    ),
                    IconButton(
                      icon: const Icon(Icons.chevron_right, size: 20),
                      visualDensity: VisualDensity.compact,
                      tooltip: 'Next slide note',
                      onPressed: () => setState(() => _slide++),
                    ),
                  ],
                ],
              ),
              const SizedBox(height: 8),
              Expanded(
                child: _notes.isEmpty
                    ? const SizedBox.shrink()
                    : SingleChildScrollView(
                        child: Text(
                          _slide - 1 < _notes.length ? _notes[_slide - 1] : '',
                          style: const TextStyle(fontSize: 15, height: 1.35),
                        ),
                      ),
              ),
              Row(
                mainAxisAlignment: MainAxisAlignment.end,
                children: _notes.isEmpty
                    ? [
                        IconButton.filledTonal(
                          icon: const Icon(Icons.note_add_outlined),
                          tooltip: 'Create notes',
                          onPressed: _openNotesEditor,
                        ),
                        const SizedBox(width: 12),
                        IconButton.filledTonal(
                          icon: const Icon(Icons.folder_open_outlined),
                          tooltip: 'Open file',
                          onPressed: _openNotesFile,
                        ),
                      ]
                    : [
                        IconButton.filledTonal(
                          icon: const Icon(Icons.edit_outlined),
                          tooltip: 'Edit notes',
                          onPressed: _openNotesEditor,
                        ),
                      ],
              ),
            ],
          ),
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final outputs = _status?.outputs ?? const <OutputInfo>[];
    final scheme = Theme.of(context).colorScheme;
    return Scaffold(
      appBar: AppBar(
        leading: IconButton(
          icon: Icon(_fullscreen ? Icons.fullscreen_exit : Icons.fullscreen),
          tooltip: _fullscreen ? 'Exit full screen' : 'Full screen',
          onPressed: _toggleFullscreen,
        ),
        actions: [
          IconButton(
            icon: const Icon(Icons.link_off),
            tooltip: 'Disconnect',
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
                  child: Text(reply, style: TextStyle(color: scheme.error)),
                );
              },
            ),
            _notesBox(scheme),
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 16, 16, 0),
              child: Row(
                children: [
                  Expanded(
                    child: SizedBox(
                      height: 92,
                      child: OutlinedButton.icon(
                        onPressed: _goPrevious,
                        icon: const Icon(Icons.skip_previous, size: 34),
                        label: const Text(
                          'Previous',
                          style: TextStyle(
                            fontSize: 19,
                            fontWeight: FontWeight.w600,
                          ),
                        ),
                      ),
                    ),
                  ),
                  const SizedBox(width: 12),
                  Expanded(
                    child: SizedBox(
                      height: 92,
                      child: FilledButton.icon(
                        onPressed: _goNext,
                        icon: const Icon(Icons.skip_next, size: 34),
                        label: const Text(
                          'Next',
                          style: TextStyle(
                            fontSize: 19,
                            fontWeight: FontWeight.w600,
                          ),
                        ),
                      ),
                    ),
                  ),
                ],
              ),
            ),
            // The laser unit: one card whose header strip carries the
            // monitor dropdown (multi-monitor setups only) and whose
            // body is the hold area — icon only, no text.
            Expanded(
              child: Padding(
                padding: const EdgeInsets.all(16),
                child: Container(
                  decoration: BoxDecoration(
                    color: _laserHeld
                        ? Colors.green.shade600
                        : Colors.green.shade900,
                    borderRadius: BorderRadius.circular(28),
                  ),
                  child: Column(
                    children: [
                      if (outputs.length > 1) ...[
                        Padding(
                          padding: const EdgeInsets.fromLTRB(20, 6, 12, 6),
                          child: Row(
                            children: [
                              const Icon(
                                Icons.monitor,
                                size: 20,
                                color: Colors.white70,
                              ),
                              const SizedBox(width: 10),
                              DropdownButton<String?>(
                                value: _confined,
                                underline: const SizedBox.shrink(),
                                dropdownColor: Colors.green.shade800,
                                iconEnabledColor: Colors.white70,
                                style: const TextStyle(
                                  color: Colors.white,
                                  fontSize: 16,
                                  fontWeight: FontWeight.w600,
                                ),
                                items: [
                                  const DropdownMenuItem<String?>(
                                    value: null,
                                    child: Text('All screens'),
                                  ),
                                  for (final o in outputs)
                                    DropdownMenuItem<String?>(
                                      value: o.name,
                                      child: Text(o.name),
                                    ),
                                ],
                                onChanged: (value) => _pickConfine(value),
                              ),
                            ],
                          ),
                        ),
                        const Divider(
                          height: 1,
                          thickness: 1,
                          color: Colors.white24,
                        ),
                      ],
                      Expanded(
                        child: Listener(
                          behavior: HitTestBehavior.opaque,
                          onPointerDown: (_) => _laserDown(),
                          onPointerUp: (_) => _laserUp(),
                          onPointerCancel: (_) => _laserUp(),
                          child: const Center(
                            child: Icon(
                              Icons.gps_fixed,
                              size: 88,
                              color: Colors.white,
                            ),
                          ),
                        ),
                      ),
                    ],
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
