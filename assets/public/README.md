# Public character runtime

The original PixelLab character is integrated in all 16 runtime states. Each
state has six or ten frames at 144 x 192 px, with a baseline of 188 px. Every
generated frame is preserved; the first and last runtime frames use the exact
approved rest pose. Permission, success, error, privacy and do-not-disturb
states add their geometric badge. Hit regions cover the union of the actual
alpha, while native per-frame hit testing follows the visible pixels at each DPI.

Generation records and licensing: [character sources](../character-source/NOTICE.md).
Reproduce the offline packaging with `python tools/build_character.py` and
check it with `python tools/build_character.py --check`. Both commands require
the public identity adapter and refuse a personal-edition root.

The 42 generic UI glyphs and eight panel textures retain the project's original
geometric design and MIT license. They are distinct from the PixelLab character,
portrait, user compass and derived icons, which have their own notices.
