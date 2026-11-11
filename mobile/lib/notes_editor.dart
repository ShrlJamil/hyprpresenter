import 'package:flutter/material.dart';

import 'notes.dart';

/// The slide-notes editor, modelled on a notebook file (ipynb-style):
/// one bordered cell per slide stacked vertically, a wide "+" tile
/// under the last cell adds another slide.  No toolbar, almost no
/// text — a numbered badge per cell is all the chrome there is.
///
/// Every keystroke is persisted to [NotesStore] immediately, so
/// leaving the page at any point loses nothing.  The app-bar actions
/// open an existing notes file and export the current notes as a
/// Markdown file.
class NotesEditorPage extends StatefulWidget {
  const NotesEditorPage({super.key});

  @override
  State<NotesEditorPage> createState() => _NotesEditorPageState();
}

class _NotesEditorPageState extends State<NotesEditorPage> {
  final List<TextEditingController> _cells = [];
  bool _loaded = false;
  int _autofocus = -1;

  @override
  void initState() {
    super.initState();
    _load();
  }

  @override
  void dispose() {
    for (final cell in _cells) {
      cell.dispose();
    }
    super.dispose();
  }

  Future<void> _load() async {
    final notes = await NotesStore.load();
    if (!mounted) return;
    setState(() {
      for (final note in notes) {
        _cells.add(TextEditingController(text: note));
      }
      if (_cells.isEmpty) _cells.add(TextEditingController());
      _loaded = true;
    });
  }

  List<String> get _values => [for (final cell in _cells) cell.text];

  void _persist() => NotesStore.save(_values);

  void _addCell() {
    setState(() {
      _cells.add(TextEditingController());
      _autofocus = _cells.length - 1;
    });
    _persist();
  }

  void _removeCell(int index) {
    setState(() {
      _cells.removeAt(index).dispose();
      if (_cells.isEmpty) _cells.add(TextEditingController());
      _autofocus = -1;
    });
    _persist();
  }

  Future<void> _openFile() async {
    final notes = await pickNotesFile();
    if (notes == null || !mounted) return;
    setState(() {
      for (final cell in _cells) {
        cell.dispose();
      }
      _cells
        ..clear()
        ..addAll(notes.map((n) => TextEditingController(text: n)));
      if (_cells.isEmpty) _cells.add(TextEditingController());
      _autofocus = -1;
    });
    _persist();
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Scaffold(
      appBar: AppBar(
        actions: [
          IconButton(
            icon: const Icon(Icons.folder_open_outlined),
            tooltip: 'Open file',
            onPressed: _loaded ? _openFile : null,
          ),
          IconButton(
            icon: const Icon(Icons.ios_share),
            tooltip: 'Export .md',
            onPressed: _loaded ? () => exportNotesFile(_values) : null,
          ),
        ],
      ),
      body: !_loaded
          ? const Center(child: CircularProgressIndicator())
          : ListView.builder(
              padding: const EdgeInsets.all(16),
              itemCount: _cells.length + 1,
              itemBuilder: (context, i) {
                if (i == _cells.length) {
                  return SizedBox(
                    height: 52,
                    child: OutlinedButton(
                      onPressed: _addCell,
                      child: const Icon(Icons.add),
                    ),
                  );
                }
                return Padding(
                  padding: const EdgeInsets.only(bottom: 12),
                  child: Container(
                    decoration: BoxDecoration(
                      border: Border.all(color: scheme.outlineVariant),
                      borderRadius: BorderRadius.circular(14),
                    ),
                    padding: const EdgeInsets.fromLTRB(12, 6, 6, 10),
                    child: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        Row(
                          children: [
                            CircleAvatar(
                              radius: 13,
                              backgroundColor: scheme.primaryContainer,
                              foregroundColor: scheme.onPrimaryContainer,
                              child: Text(
                                '${i + 1}',
                                style: const TextStyle(
                                  fontSize: 12,
                                  fontWeight: FontWeight.w700,
                                ),
                              ),
                            ),
                            const Spacer(),
                            IconButton(
                              icon: const Icon(Icons.close, size: 18),
                              visualDensity: VisualDensity.compact,
                              onPressed: () => _removeCell(i),
                            ),
                          ],
                        ),
                        Padding(
                          padding: const EdgeInsets.only(left: 6, right: 6),
                          child: TextField(
                            controller: _cells[i],
                            autofocus: i == _autofocus,
                            minLines: 2,
                            maxLines: null,
                            decoration: const InputDecoration(
                              border: InputBorder.none,
                              isDense: true,
                            ),
                            onChanged: (_) => _persist(),
                          ),
                        ),
                      ],
                    ),
                  ),
                );
              },
            ),
    );
  }
}
