/// Gyroscope laser pointer engine (Phase 6.2).
///
/// The "pointing" model the product specifies: when the laser button is
/// pressed, the phone's current orientation becomes the reference and the
/// dot starts at the centre of the active screen area.  While the button
/// is held, the dot offset follows the phone's orientation *deviation*
/// from that reference — tilt the phone back to its starting pose and the
/// dot returns to the centre.  Releasing hides the laser and the next
/// press re-anchors from scratch.
///
/// Orientation is tracked with a complementary filter: gyroscope samples
/// are integrated for responsiveness, accelerometer samples continually
/// pull the pitch/roll estimate back toward the measured gravity
/// direction so it cannot drift.  (Yaw has no absolute reference from
/// these sensors, but a hold lasts seconds and every press re-anchors,
/// so yaw drift never accumulates in practice.)
library;

import 'dart:math' as math;

/// A unit quaternion (w, x, y, z) mapping device-frame vectors to the
/// world frame: `v_world = q * v_device * q*`.
class Quat {
  const Quat(this.w, this.x, this.y, this.z);

  final double w;
  final double x;
  final double y;
  final double z;

  static const Quat identity = Quat(1, 0, 0, 0);

  Quat operator *(Quat o) => Quat(
    w * o.w - x * o.x - y * o.y - z * o.z,
    w * o.x + x * o.w + y * o.z - z * o.y,
    w * o.y - x * o.z + y * o.w + z * o.x,
    w * o.z + x * o.y - y * o.x + z * o.w,
  );

  Quat get conjugate => Quat(w, -x, -y, -z);

  Quat normalized() {
    final n = math.sqrt(w * w + x * x + y * y + z * z);
    if (n == 0) return identity;
    return Quat(w / n, x / n, y / n, z / n);
  }

  /// Rotate vector (vx, vy, vz) by this quaternion.
  ({double x, double y, double z}) rotate(double vx, double vy, double vz) {
    final r = this * Quat(0, vx, vy, vz) * conjugate;
    return (x: r.x, y: r.y, z: r.z);
  }

  static Quat fromAxisAngle(double ax, double ay, double az, double angle) {
    final half = angle / 2;
    final s = math.sin(half);
    return Quat(math.cos(half), ax * s, ay * s, az * s);
  }

  /// The rotation vector (axis * angle, radians) of this quaternion.
  ({double x, double y, double z}) get rotationVector {
    final wClamped = w.clamp(-1.0, 1.0);
    final angle = 2 * math.acos(wClamped);
    final s = math.sqrt(1 - wClamped * wClamped);
    if (s < 1e-6) {
      // Small angle: rotation vector ≈ 2 * vector part.
      return (x: 2 * x, y: 2 * y, z: 2 * z);
    }
    final scale = angle / s;
    return (x: x * scale, y: y * scale, z: z * scale);
  }
}

/// Tracks phone orientation and reports deviations from a press anchor.
class GyroPointer {
  Quat _q = Quat.identity;
  Quat? _anchor;
  DateTime? _lastGyroAt;

  /// Fraction of the gravity-alignment error corrected per accelerometer
  /// sample (complementary-filter gain; samples arrive ~50-200 Hz).
  static const double _gravityGain = 0.02;

  /// Feed one gyroscope sample (angular velocity, rad/s, device frame).
  void addGyroscope(double wx, double wy, double wz, DateTime now) {
    final last = _lastGyroAt;
    _lastGyroAt = now;
    if (last == null) return;
    var dt = now.difference(last).inMicroseconds / 1e6;
    if (dt <= 0) return;
    if (dt > 0.1) dt = 0.1;
    final magnitude = math.sqrt(wx * wx + wy * wy + wz * wz);
    final angle = magnitude * dt;
    if (angle < 1e-9) return;
    final inv = 1 / magnitude;
    _q = (_q * Quat.fromAxisAngle(wx * inv, wy * inv, wz * inv, angle))
        .normalized();
  }

  /// Feed one accelerometer sample (m/s², device frame).  At rest this
  /// vector points along world "up" (opposite gravity).
  void addAccelerometer(double ax, double ay, double az) {
    final n = math.sqrt(ax * ax + ay * ay + az * az);
    if (n < 1e-6) return;
    final mx = ax / n, my = ay / n, mz = az / n;
    // Predicted up direction in the device frame.
    final pred = _q.conjugate.rotate(0, 0, 1);
    // Correction (device frame): rotating q by Δq rotates the prediction
    // by Δq⁻¹, so Δq must rotate measured → predicted; its axis is
    // measured × predicted and only a gain fraction is applied.
    var cx = my * pred.z - mz * pred.y;
    var cy = mz * pred.x - mx * pred.z;
    var cz = mx * pred.y - my * pred.x;
    final cl = math.sqrt(cx * cx + cy * cy + cz * cz);
    if (cl < 1e-9) return;
    final angle = math.asin(cl.clamp(-1.0, 1.0)) * _gravityGain;
    cx /= cl;
    cy /= cl;
    cz /= cl;
    _q = (_q * Quat.fromAxisAngle(cx, cy, cz, angle)).normalized();
  }

  /// Mark the current orientation as the reference (button pressed).
  void anchor() {
    _anchor = _q;
    _lastGyroAt = null;
  }

  /// Orientation deviation from the anchor, as the deflection of the
  /// phone's top edge (device +Y, the "barrel" you aim with) from its
  /// anchored direction, decomposed in the anchor frame:
  /// `yaw` = deflection toward the anchor's X axis (leaning/aiming the
  /// top edge sideways — the natural horizontal steering motion, mostly
  /// roll), `pitch` = deflection toward the anchor's Z axis (nodding the
  /// top edge toward/away).  Both are atan2 angles in radians.
  ///
  /// This replaced a rotation-vector decomposition after on-device
  /// trials: reading only the rotation vector's X/Y components made
  /// horizontal steering feel walled-in around the centre, because the
  /// hand steers sideways mostly by rolling the phone (rotation about
  /// Z), which that decomposition throws away.  The barrel deflection
  /// captures pitch and roll aiming alike; spinning the phone about its
  /// own long axis correctly steers nothing.
  ({double pitch, double yaw})? get deviation {
    final anchor = _anchor;
    if (anchor == null) return null;
    final d = _q.rotate(0, 1, 0);
    final d0 = anchor.rotate(0, 1, 0);
    final r0 = anchor.rotate(1, 0, 0);
    final s0 = anchor.rotate(0, 0, 1);
    double dot(
      ({double x, double y, double z}) a,
      ({double x, double y, double z}) b,
    ) => a.x * b.x + a.y * b.y + a.z * b.z;
    final forward = dot(d, d0);
    return (
      pitch: math.atan2(dot(d, s0), forward),
      yaw: math.atan2(dot(d, r0), forward),
    );
  }

  /// The estimated "up" direction in the device frame (test/debug aid).
  ({double x, double y, double z}) get predictedUp =>
      _q.conjugate.rotate(0, 0, 1);
}
