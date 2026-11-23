Put an image of your Future Cop: L.A.P.D. CD (.iso or .bin) next to
fcop.exe (Linux: fcop), or put your copy of the game (PC version) into this folder:
the contents of the CD, an image of the CD (.iso or .bin), or your
installed game folder.

At its next start fcop finds the game, checks that it is the version
this port knows, and keeps only what the game needs:

  FCopLAPD\   FCopLAPD.exe, febmp.bin, missions (and your settings,
              key bindings and saved games)
  Video\      the videos (from the CD; the game runs without them)
  Textures\   replacement textures, if you have any (they are kept)
              ("fcop export" cuts the game's own into pictures to start from)
  PS1\        FE.MIS from the PlayStation version, if you have it: its menu
              music, which the PC version left out (optional)

The PlayStation menu music: put an image of your PlayStation CD (.bin
with its .cue, or .iso) into this folder, or copy the file CW\FE.MIS
from that CD into the folder PS1 here (PS1\FE.MIS). An image next to
fcop.exe or FE.MIS elsewhere in this folder is found too. From the image
only FE.MIS is taken out. Without it the menus stay silent, as on the PC.

From a CD image these are taken out. Everything else in this folder
(the image too) is moved to the recycle bin (Linux: deleted).
