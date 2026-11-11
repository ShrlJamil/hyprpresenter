import 'dart:math' as math;

import 'package:flutter_test/flutter_test.dart';
import 'package:hyprpresenter_mobile/gyro_pointer.dart';

void main() {
  group('GyroPointer', () {
    test('deviation is null before the first anchor', () {
      final pointer = GyroPointer();
      expect(pointer.deviation, isNull);
    });

    test('integrating a steady yaw rate yields that yaw deviation', () {
      final pointer = GyroPointer();
      var t = DateTime.fromMillisecondsSinceEpoch(0);
      // Prime the integrator clock, then anchor (anchor resets the clock).
      pointer.addGyroscope(0, 0, 0, t);
      pointer.anchor();
      pointer.addGyroscope(0, 0, 0, t);
      // 0.6 rad/s about device Y for exactly 1 second, in 10 ms steps.
      for (var i = 0; i < 100; i++) {
        t = t.add(const Duration(milliseconds: 10));
        pointer.addGyroscope(0, 0.6, 0, t);
      }
      final dev = pointer.deviation!;
      expect(dev.yaw, closeTo(0.6, 0.03));
      expect(dev.pitch, closeTo(0, 0.02));
    });

    test('integrating a steady pitch rate yields that pitch deviation', () {
      final pointer = GyroPointer();
      var t = DateTime.fromMillisecondsSinceEpoch(0);
      pointer.addGyroscope(0, 0, 0, t);
      pointer.anchor();
      pointer.addGyroscope(0, 0, 0, t);
      for (var i = 0; i < 100; i++) {
        t = t.add(const Duration(milliseconds: 10));
        pointer.addGyroscope(-0.5, 0, 0, t);
      }
      final dev = pointer.deviation!;
      expect(dev.pitch, closeTo(-0.5, 0.03));
      expect(dev.yaw, closeTo(0, 0.02));
    });

    test('gravity correction converges to a tilted resting pose', () {
      final pointer = GyroPointer();
      // Device pitched +20 degrees about X: up in the device frame.
      final tilt = 20 * math.pi / 180;
      final ax = 0.0;
      final ay = math.sin(tilt) * 9.81;
      final az = math.cos(tilt) * 9.81;
      for (var i = 0; i < 500; i++) {
        pointer.addAccelerometer(ax, ay, az);
      }
      final up = pointer.predictedUp;
      expect(up.x, closeTo(0, 0.02));
      expect(up.y, closeTo(math.sin(tilt), 0.02));
      expect(up.z, closeTo(math.cos(tilt), 0.02));
      // Anchoring in that pose reports (near) zero deviation.
      pointer.anchor();
      final dev = pointer.deviation!;
      expect(dev.pitch, closeTo(0, 0.02));
      expect(dev.yaw, closeTo(0, 0.02));
    });
  });
}
