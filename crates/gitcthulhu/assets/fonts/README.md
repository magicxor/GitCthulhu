# Fonts

Inter 4.001 is the UI font (https://github.com/rsms/inter). The files here are static instances of its
variable font, cut with fontTools:

```bash
fonttools ttLib.woff2 decompress Inter-Variable.woff2 -o Inter-Variable.ttf
fonttools varLib.instancer Inter-Variable.ttf wght=400 --update-name-table -o Inter-Regular.ttf
```

Each file is 60 lighter than the weight it stands in for: Regular is 400 for 460, Medium 440 for 500 and
SemiBold 540 for 600, because glyphs rasterize slightly heavier than in a browser. Weights 440 and 540 have no
named instance, so those files are cut without `--update-name-table` and keep the name table and OS/2 weight
class of the 500 and 600 instances, which GPUI matches weights by.

The cut-down Inter leaves out the symbols the app's keys are written with (⌘ ⌥ ⇧ ⌃ ↩ ⌫ ⌦ ⇥, and it
has ← and → only for its `->` arrow), and without them CoreText draws those from another font,
heavier and off the baseline. Each Inter file adds them from the full Inter 4.001
(https://github.com/rsms/inter), instanced at optical size 14, and at the weight the file was cut at, leaving every other glyph as
it was:

```python
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer

full = instancer.instantiateVariableFont(TTFont("InterVariable.ttf"), {"opsz": 14, "wght": 400})
base, order = TTFont("Inter-Regular.ttf"), []
for ch in "←→↩⇥⇧⌃⌘⌥⌦⌫":
    name = full.getBestCmap()[ord(ch)]
    if name not in base.getGlyphOrder() + order:
        order.append(name)
        base["glyf"].glyphs[name] = full["glyf"][name]  # none of them is a composite
        base["hmtx"].metrics[name] = full["hmtx"].metrics[name]
    for table in base["cmap"].tables:
        if table.isUnicode():
            table.cmap[ord(ch)] = name
base.setGlyphOrder(base.getGlyphOrder() + order)
base["glyf"].glyphOrder = base.getGlyphOrder()
base.save("Inter-Regular.ttf")
```

GPUI keeps a font added as bytes at its default instance, so it never reaches a variable file's
other weights, hence static files. Inter is under the SIL Open Font License 1.1 (https://openfontlicense.org); `LICENSE-Inter.txt` holds its
copyright notice and the license text. The files here are Modified Versions (re-instanced weights, added
glyphs); both license texts (`LICENSE-Inter.txt`, `LICENSE-GeistMonoNerdFont.txt`) are compiled into the
application and shown in its About dialog, and must be copied into any redistributed package.

## Code and terminal font

Code and terminals draw with Geist Mono Nerd Font Mono (`GeistMonoNerdFontMono-*.otf`), Geist Mono
1.702 patched by Nerd Fonts v3.5.1, unmodified:

```bash
gh release download v3.5.1 -R ryanoasis/nerd-fonts -p GeistMono.tar.xz
tar xf GeistMono.tar.xz GeistMonoNerdFontMono-Regular.otf GeistMonoNerdFontMono-SemiBold.otf LICENSE
```

The Geist Mono in but-ui is a subset. It leaves out most accented Latin letters, as 16 of
Polish's 18, and the box-drawing characters and Private Use Area file icons TUIs draw with.
CoreText takes those from other fonts with other designs and widths, so words mix two fonts,
rows drift off the grid, borders gap, and icons show as missing glyphs. The Mono variant keeps
every glyph, icons included, one cell wide. Nerd Fonts ships only these fixed weights, so bold is
600 rather than 540, and it has no ligatures, so the Font ligatures setting shows only in a font
the user picks. CoreText names the family "GeistMono Nerd Font Mono", not the "GeistMono NFM" in
the files' legacy names.

Nerd Fonts' patched fonts are under the SIL Open Font License 1.1
([licensing](https://github.com/ryanoasis/nerd-fonts/blob/v3.5.1/LICENSE)), and
`LICENSE-GeistMonoNerdFont.txt` is the license from the release archive.
