import 'dart:math' as math;

import 'package:flutter_test/flutter_test.dart';
import 'package:hyprpresenter_mobile/gyro_pointer.dart';

/// Feed a constant angular velocity (rad/s) for exactly one second.
void spin(GyroPointer pointer, double wx, double wy, double wz) {
  var t = DateTime.fromMillisecondsSinceEpoch(0);
  pointer.addGyroscope(0, 0, 0, t);
  pointer.anchor();
  pointer.addGyroscope(0, 0, 0, t);
  for (var i = 0; i < 100; i++) {
    t = t.add(const Duration(milliseconds: 10));
    pointer.addGyroscope(wx, wy, wz, t);
  }
}

void main() {
  group('GyroPointer', () {
    test('deviation is null before the first anchor', () {
      expect(GyroPointer().deviation, isNull);
    });

    test('rolling the phone steers horizontally (top edge lean)', () {
      final pointer = GyroPointer();
      // +0.6 rad about Z leans the top edge toward -X in the anchor frame.
      spin(pointer, 0, 0, 0.6);
      final dev = pointer.deviation!;
      expect(dev.yaw, closeTo(-0.6, 0.03));
      expect(dev.pitch, closeTo(0, 0.02));
    });

    test('nodding the phone steers vertically', () {
      final pointer = GyroPointer();
      spin(pointer, 0.5, 0, 0);
      final dev = pointer.deviation!;
      expect(dev.pitch, closeTo(0.5, 0.03));
      expect(dev.yaw, closeTo(0, 0.02));
    });

    test('spinning about the long axis steers nothing', () {
      final pointer = GyroPointer();
      spin(pointer, 0, 0.6, 0);
      final dev = pointer.deviation!;
      expect(dev.yaw, closeTo(0, 0.02));
      expect(dev.pitch, closeTo(0, 0.02));
    });

    test('gravity correction converges to a tilted resting pose', () {
      final pointer = GyroPointer();
      final tilt = 20 * math.pi / 180;
      for (var i = 0; i < 500; i++) {
        pointer.addAccelerometer(
          0,
          math.sin(tilt) * 9.81,
          math.cos(tilt) * 9.81,
        );
      }
      final up = pointer.predictedUp;
      expect(up.x, closeTo(0, 0.02));
      expect(up.y, closeTo(math.sin(tilt), 0.02));
      expect(up.z, closeTo(math.cos(tilt), 0.02));
      pointer.anchor();
      final dev = pointer.deviation!;
      expect(dev.pitch, closeTo(0, 0.02));
      expect(dev.yaw, closeTo(0, 0.02));
    });
  });
}
