/// Parsing for the desktop's `status` reply (Phase 6.2):
/// `dot=.. on=.. visible=.. radius=.. color=.. confined=.. areas=NAME:X,Y,W,H;..`
library;

/// One monitor's rectangle in global logical coordinates.
class OutputInfo {
  const OutputInfo(this.name, this.x, this.y, this.width, this.height);

  final String name;
  final int x;
  final int y;
  final int width;
  final int height;

  int get centerX => x + width ~/ 2;
  int get centerY => y + height ~/ 2;

  @override
  String toString() => '$name:$x,$y,$width,$height';
}

/// The parts of a status reply the app acts on.
class DesktopStatus {
  const DesktopStatus({
    required this.outputs,
    required this.confined,
    required this.onOutput,
    required this.dotX,
    required this.dotY,
  });

  final List<OutputInfo> outputs;

  /// Output the laser is confined to, or null when it roams freely.
  final String? confined;

  /// Output the dot currently sits on, if any.
  final String? onOutput;
  final int dotX;
  final int dotY;

  OutputInfo? outputNamed(String? name) {
    if (name == null) return null;
    for (final o in outputs) {
      if (o.name == name) return o;
    }
    return null;
  }

  /// Union bounds of all outputs as (minX, minY, maxX, maxY), max exclusive.
  ({int minX, int minY, int maxX, int maxY}) get bounds {
    var minX = outputs.first.x;
    var minY = outputs.first.y;
    var maxX = outputs.first.x + outputs.first.width;
    var maxY = outputs.first.y + outputs.first.height;
    for (final o in outputs.skip(1)) {
      if (o.x < minX) minX = o.x;
      if (o.y < minY) minY = o.y;
      if (o.x + o.width > maxX) maxX = o.x + o.width;
      if (o.y + o.height > maxY) maxY = o.y + o.height;
    }
    return (minX: minX, minY: minY, maxX: maxX, maxY: maxY);
  }
}

/// Parse a status reply line, or null when it does not look like one.
DesktopStatus? parseDesktopStatus(String line) {
  final fields = <String, String>{};
  for (final part in line.trim().split(RegExp(r'\s+'))) {
    final eq = part.indexOf('=');
    if (eq > 0) fields[part.substring(0, eq)] = part.substring(eq + 1);
  }
  final areas = fields['areas'];
  if (areas == null || areas.isEmpty) return null;
  final outputs = <OutputInfo>[];
  for (final entry in areas.split(';')) {
    final colon = entry.indexOf(':');
    if (colon <= 0) return null;
    final nums = entry
        .substring(colon + 1)
        .split(',')
        .map(int.tryParse)
        .toList();
    if (nums.length != 4 || nums.any((n) => n == null)) return null;
    outputs.add(
      OutputInfo(
        entry.substring(0, colon),
        nums[0]!,
        nums[1]!,
        nums[2]!,
        nums[3]!,
      ),
    );
  }
  if (outputs.isEmpty) return null;
  final dot = (fields['dot'] ?? '0,0').split(',').map(int.tryParse).toList();
  final confinedRaw = fields['confined'];
  return DesktopStatus(
    outputs: outputs,
    confined: (confinedRaw == null || confinedRaw == 'none')
        ? null
        : confinedRaw,
    onOutput: fields['on'] == 'none' ? null : fields['on'],
    dotX: dot.isNotEmpty && dot[0] != null ? dot[0]! : 0,
    dotY: dot.length > 1 && dot[1] != null ? dot[1]! : 0,
  );
}
