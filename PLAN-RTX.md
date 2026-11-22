# Plan — Vulkan, DLSS, RTX raytracing

Started 2026-11-21. Experimental, kept apart from PLAN.md. The main plan stays as it is.

Goal (the user's): modernize the renderer for RTX raytracing and DLSS. DLSS 5 needs no
work of its own: managers (NVIDIA App override, DLL swappers) hook into an existing DLSS
integration. Our job is a clean standard integration that they can find.

Hardware here: RTX 4090 (Ada). It supports DLSS Super Resolution, DLAA, Frame Generation
(2x), Ray Reconstruction and Reflex. It has no Multi Frame Generation, which needs RTX 50.

## Where we stand (2026-11-21, branch `feat/vulkan`, not pushed)

Step 1 done (Vulkan presents, GL draws into shared images). Step 2: the scene in world space
(models, animated parts, terrain, the whole level) and the first BLAS/TLAS with a ray query
debug picture (`FCOP_VULKAN=1 FCOP_GBUFFER=rt`, `FCOP_RT_HIGH=<units>` from above).

Sun shadows by ray query work (2026-11-21, `Shadows` 3 "Ray Traced", step 5's first item):
- Synchronous, not one picture late: per view in `glide::post_3d` GL copies the depth into a
  shared RG32F image (R), signals; Vulkan builds the scene and traces (G = blocked), signals
  back; GL waits on the GPU and the AO's last pass uses G. One image for both ways, two
  semaphores. No lag when turning; cheaper than feared.
- Looks like the shadow map (compared at the first mission's start), without its ring and with
  the whole level casting. ~530 fps against the map's ~320 (1280x960, MSAA 8, AO on).
- Validation clean.
- Alpha test (2026-11-21): keyed texels (railings, the ramp's lattice walls, which are terrain,
  trees) let the light through, as with the shadow map. Triangle materials from guest memory
  (models' `3DTL`, terrain's texture words), the mission's pages as coverage bits; only
  triangles whose texel box has a black texel are tested (a geometry of their own in each
  BLAS: making whole BLAS non-opaque halved the frame rate, ~540 -> ~280; split ~540).
- Soft edges (2026-11-22): 4 rays a pixel into a sun disc of 1° radius (points on rings of equal
  area, turned and shifted per pixel by interleaved gradient noise), G = the share blocked,
  filtered over 5x5 pixels of the same depth in the AO's last pass. Penumbras grow with the
  distance to the caster; 1° keeps a lattice's stripes, 2° washes them out; 1 ray was blotchy.
  `FCOP_RT_SOFT=<degrees>` (0 hard), `FCOP_RT_RAYS=<n>` to compare.
- GPU times (`FCOP_RT_TIME=1`, timestamps, first mission start 1280x960): the scene's build
  (TLAS, instances) ~98 µs a view, the rays ~42 µs a ray a pixel (hard 47, 4 rays 170).
- Dynamic lights' shadows by rays (2026-11-22): with `Shadows` 3 and the lights on, the sun's
  shader also traces the 8 strongest merged lights (`lights::casters`, a storage buffer, binding
  4): one hard ray from each pixel within a light's reach toward it, ending 0.3 of the reach short
  (the light's place is pulled toward the camera by up to 0.2 of it; with 0.08 a robot's own
  police lights left the ground around it dark). Bits in G above the sun's share (G = share + 2 *
  bits, exact in a float); the lights' pass takes them instead of its screen-space march. The legs
  cast shadows of the light bar. +~10 µs a view, ~300 -> ~338 fps (1920x1080, the march is
  gone). `FCOP_RT_LIGHTS=0` (a test) marches again.
- TLAS refit (2026-11-22): the ~98 µs were all the TLAS build (~310 instances: 169 terrain
  blocks, ~150 models; ~30 µs even for 10 instances, coincident boxes far worse). Now built with
  ALLOW_UPDATE and refitted (UPDATE in place) while the instances name the same BLAS in the same
  order as at its last build (~95% of pictures: the game's lists keep their order; a refit with
  other BLAS in a slot lost the device), anew on any change and every 30 pictures: ~24 µs a view,
  the rays as fast as before. Tried without effect: instances in device memory, PREFER_FAST_BUILD
  (~92), the build before GL's semaphore (GL's and Vulkan's work do not overlap on the GPU).
  GPU time counts here: the sun's rays are synchronous (~150 µs less GPU time gave ~402 -> ~432
  fps); fps from run to run varied by ±7%, the timestamps (`FCOP_RT_TIME=1`, now with the TLAS
  alone and the instance counts) are the measure.

Next:
- Alpha test open points: scrolled terrain textures (tw & 0x800) are tested unscrolled;
  semi-transparent faces (glass, PS1 STP) still cast fully; HD replacement textures' coverage
  is not used (the original page's).
- Compare in more levels and at night/evening suns (`sun` cheat); the two dark lines at block
  borders seen from above (step 2) may show as thin shadow lines.

## Principles

- Every step stays playable. The OpenGL renderer stays the default until Vulkan gives the
  same picture. Vulkan is switched on by `FCOP_VULKAN=1` first, later by a setting.
- OpenGL has no raytracing, and Streamline supports only D3D11/D3D12/Vulkan. So the
  target is Vulkan through `ash`, on Windows and Linux (later Android too).
- The game projects on the CPU and sends 2D points to Glide. Raytracing needs the scene in
  world space, also the parts that are not on screen. We build that scene from the game's
  own data (models, terrain, actor and object lists), not from screen points.
- Motion vectors come from that scene (each instance's transform now and in the last
  picture). DLSS, Frame Generation and every RT denoiser need the same groundwork.

## Step 1: Vulkan base, present through Vulkan

- [x] `ash` instance and device on the winit window (surface from raw-window-handle).
      The RT extensions (`VK_KHR_acceleration_structure`, `VK_KHR_ray_query`,
      `VK_KHR_ray_tracing_pipeline`) are enabled when present, never required.
- [x] Swapchain: FIFO for V-Sync, MAILBOX or IMMEDIATE without. It is made again on
      resize, on fullscreen (Alt+Enter) and when the window is minimized.
- [x] GL interop: GL renders the finished picture (HUD and dialog overlay included) into an
      image that both APIs share (`GL_EXT_memory_object_win32` / `_fd`,
      `GL_EXT_semaphore`). Vulkan copies it into the swapchain image and presents it.
- [x] The GL context then lives on a hidden window of its own (Windows tested; Linux not
      yet). The Linux splash (`fcop --splash`) keeps `gl.rs`.
- [x] Pitfall found (2026-11-21): NVIDIA's GL takes imported memory by its Win32 handle only
      at glTexStorageMem2DEXT. Closed right after the import, GL silently drew into memory of
      its own: a black window at ~40 fps, no GL error, validation clean. The handle is closed
      after the texture storage now; the two semaphore handles stay open.
- [ ] Speed: first mission start, 1280x960, V-Sync off: ~435 fps with Vulkan against ~557
      with GL. The cost is NVIDIA's GL: glWaitSemaphoreEXT blocked ~300 µs a picture,
      glSignalSemaphoreEXT blocks until the GPU finished the picture (~350 µs). A ring of 3
      shared images (2026-11-21) made the wait unnecessary (the CPU already waited for the
      copy's fence), but the frame rate stayed: the block moved into the signal. Left open:
      signal, submit and present on a thread of their own (a second GL context in the share
      group, glFenceSync / glWaitSync from the main one) would hide ~0.5 ms a picture. Step 6
      (everything in Vulkan) removes the interop and this cost anyway.
- [ ] Linux (EXT_memory_object_fd, the hidden window under Wayland/X11) in the CachyOS VM.
- [ ] Check: same picture as GL (`FCOP_SHOT`, compared pixel by pixel), frame times with
      `FCOP_FRAMELOG` and CapFrameX, V-Sync pacing as in `fps.rs` (memory: losses at 480 Hz
      were inside SwapBuffers; measure present the same way).

## Step 2: the scene in world space

What the game draws comes from a few sources. Each one becomes acceleration structures:

- [ ] Models: `Cobj` (`mission.rs` reads vertices `4DVL`, faces `3DQL`, UVs `3DTL`). One BLAS
      per model, built once per mission. Quads become two triangles. Animated models
      (`3DHY`/`3DMI`/`3DHS`/`AnmD`: several vertex lists) get one BLAS per vertex list, or
      a refit when a frame changes.
- [ ] Terrain: `Ctil` sections (tiles, heights, shapes; read as `terrain.rs` does). One
      static BLAS per mission. Find out if any terrain changes during a mission.
      - [x] Drawn tiles in world space (2026-11-21): our terrain pass hands each block's
        camera-space origin and axes (column, row, height / 256; 16 terrain units to a model
        unit) to `scene::terrain_block`, which takes the camera out; each kept tile notes its 4
        corners (indices into the block's points, 3 layers each; a triangle repeats the first).
        In the overlay (yellow) they lie on the ground and walls.
      - [ ] The whole level, not only drawn tiles: blocks without a projection need their
        frame from the map ([0x4a31ac]) and the block steps (axes are the same everywhere).
- [ ] Instances: who is drawn, with which model and which transform.
      - Actors: list 0x50e428, count [0x50e420], matrix +0x2c, position +0x50 (`fps.rs`).
      - The robot's side weapons and lights: data [+0xac] at +0x178, +0x1c4, +0x210.
      - Static objects: grid [0x4a13fc] (`shadows.rs` step 2).
      - [x] model_draw 0x4280a0 understood and hooked (`scene.rs`, 2026-11-21): record +0
        model, +4 matrix 3x3 (i32 16.16), +0x28 position (20.12), +0x48 frame (i16). The model
        points at its chunks in guest memory: +0xc flags (0x2000000 in parts), [+0x1c]+4 the
        `4DVL` count with 8-byte vertices behind, +0x24 the `3DQL` count with 12-byte faces
        behind, +0x28 `3DTL`, +0x34 bounding spheres. Checked: all 37 models drawn at the first
        mission's start match a `Cobj` of the mission file in vertex and face count
        (`FCOP_SCENE_LOG=1`). BLAS can be built from guest memory, keyed by the model pointer.
      - [x] World space checked numerically (2026-11-21): world = v * R + p / 16 (row vectors,
        model units), camera space = (world - c / 16) * C with the camera [0x511f48] position
        c at +0x7c (20.12) and matrix C at +0x28 (16.16). Against the game's camera-space
        points in project_points (models not in parts, ~1 million points per 2 s while
        walking): mean 1.35, max 2.5 model units, the game's integer rounding.
      - [x] Models in parts (flags 0x2000000: robot, animated things; 2026-11-21): taken
        where project_points projects them: the game's camera-space matrix (0x4a05c0 + 0x30 *
        part number, computed by 0x428380 with the animation) with the camera taken out is
        the part's world matrix; no animation code of ours. Checked on models not in parts
        against their records: position 0.4 model units off on average, matrix 0.0002.
        A model not in parts takes its frame's vertex list ([[+0x1c] + 4 + 4 * frame]).
        ~25000 part instances per second at the first mission's start.
      - The whole level is needed, not only the visible part: shadows and reflections come
        from things off screen too. The ring of `shadows.rs` shows how to reach them.
- [ ] Each instance gets a stable key (actor or object address plus model), so its last
      transform is known. Mind that a destroyed actor's memory may be reused (`fps.rs`).
- [ ] Materials: texture page and UVs per face from `3DTL`, pages from `Cbmp` (and HD
      replacements, `hd.rs`), chroma key as alpha test. Colour byte and flags per face.
- [ ] TLAS built per picture. Pictures drawn in between (`fps.rs`) use the interpolated
      transforms, so the scene must be read after `fps.rs` has placed things.
- [x] Debug view `FCOP_GBUFFER=scene` (2026-11-21): each view's instances as green lines
      over its 3D picture (`scene::overlay` from `glide::post_3d`, no depth test). They lie on
      the robot (all parts, animated), enemies and objects. Terrain not yet.

- [x] First BLAS/TLAS (2026-11-21, `raytrace.rs`, `FCOP_GBUFFER=rt` with `FCOP_VULKAN=1`):
      a BLAS per model vertex range / part and per terrain block the first time it is seen
      (cached until the mission changes), a TLAS of the view's instances rebuilt each picture
      in place, a compute shader (`shaders/rt_debug.comp`, ray query, SPIR-V checked in) shoots
      a ray per pixel from the game's camera; the present shows that picture. Robot, enemies,
      objects, ground and walls stand where the game draws them. ~390 fps (~435 without).
      Validation clean. RT extensions and features are enabled only with this switch.
- [x] The whole level for the rays (2026-11-21): every block of the terrain map (header +4
      width, +8 rows; the map's world place from the first block render_terrain draws, 16 point
      steps a block; blocks are shared by many cells: one BLAS each, an instance per cell), the
      static objects of their grid and the actors (record at +0x28) the game did not draw this
      picture, by their records (parts in the pose their vertices have). What the game drew
      stays exact (animated). With the shadows on, their ring already draws every static object
      and nearly every actor of the first mission; with them off 100 objects and 18 actors come
      from the records. Seen from above (`FCOP_RT_HIGH=<units>`, a test) the map ends at its
      edges and the objects stand in the level.
- [ ] Two thin dark lines along block borders seen from above: points without a height at a
      block's edge, or a small offset of the map's place. To be checked.
- [ ] Stable instance keys (record address + part) for last picture's transforms.

## Step 3: motion vectors and jitter

- [ ] Motion vectors per pixel: our scene rasterized with this picture's and the last
      picture's transforms and cameras. Used where its depth matches the game's depth.
      Elsewhere (effects, sprites, things the scene lacks) only the camera's motion from
      the depth.
- [ ] Vectors between shown pictures, not between game frames (pictures come between frames).
- [ ] Jitter: a Halton (2, 3) subpixel offset added in the projection hooks (0x457340
      objects, 0x453740 terrain, 0x457140 sprites) in 2560x1920 units. The HUD stays still.
- [ ] Check: `FCOP_GBUFFER=motion` (colour by direction). A reprojected last picture must
      lie on the new one when walking and turning.

## Step 4: DLSS through Streamline

- [ ] Streamline (open source) with the Vulkan path: `sl.interposer`, features DLSS (Super
      Resolution and DLAA), DLSS-G (Frame Generation) and Reflex.
- [ ] Inputs: colour without the HUD, depth, motion vectors, jitter, camera matrices, and
      for Frame Generation the HUD separately (colour and alpha). The HUD is already drawn
      after `post_3d`.
- [ ] Order of the passes: the 3D picture and AO/lights/shadows at the render size, then
      DLSS to the window's size, then bloom, tone mapping and HUD. Find out if DLSS should
      get HDR values (before tone mapping, `tonemap.rs`).
- [ ] Reflex markers: simulation = the game's frame (0x407360), render and present per
      picture (`fps.rs` draws several pictures per frame).
- [ ] Settings in Advanced Graphics: DLSS (Off, DLAA, Quality, Balanced, Performance),
      Frame Generation (box), Reflex (box). MSAA is off while DLSS is on.
- [ ] DLSS 5 by managers: the standard DLL names beside fcop.exe (`nvngx_dlss.dll`,
      `nvngx_dlssg.dll`) and the Streamline path, so the NVIDIA App override finds them.
- [ ] Licence: check what the release may ship (Streamline's and the DLSS DLLs' terms).
      The DLLs never go into the repository.

## Step 5: raytracing

In this order, each with its own setting, each compared against the current effect:

- [x] Sun shadows by ray query: per pixel the world position from the depth, one ray to the
      sun against the TLAS (2026-11-21, `Shadows` 3, see "Where we stand").
- [x] Keyed texels by an alpha test in the ray query (2026-11-21).
- [x] Soft edges (2026-11-22).
- [x] A setting (2026-11-22): "Ray Traced" always in the Shadows combo beside Screen-Space and
      Shadow Map, which stays (the user: never remove it). Vulkan presents from the next start
      when chosen; until then (or without ray queries) the map is drawn. Its switches in
      Advanced Graphics too: RT Softness, RT Rays / Pixel, Ray-Traced Light Shadows (the
      `FCOP_RT_SOFT` / `_RAYS` / `_LIGHTS` variables only override them).
- [x] Shadows of the dynamic lights (`lights.rs`) by rays instead of the screen ray march
      (2026-11-22, hard; soft edges by the light's size would need a filter like the sun's).
- [x] RT ambient occlusion instead of SSAO (2026-11-22): with the sun's rays, 4 cosine-weighted
      rays a pixel into the hemisphere (normal from the depth), 400 units long, B of the shared
      image; blurred along its surface (17 pixels across and down, rt_ao_blur.comp: a 7x7
      filter alone was grainy), squared. Sees occluders off the screen and behind others;
      no SSAO halos at depth edges. ~+290 µs a view (1280x960). Needs Ray Traced shadows: an AO
      by rays alone (Vulkan without the sun) is open.
- [ ] Reflections (wet streets, water surfaces, if the levels have them).
- [ ] Global illumination, at the end path tracing with Ray Reconstruction (4090 supports it).

## Step 6: everything in Vulkan

- [ ] Port the GL passes one by one: Glide batches (`glide.rs`), AO, lights, shadows,
      bloom, heat, tone mapping, HD textures, dialog overlay. Shaders from GLSL to SPIR-V
      at build time (choose between naga and shaderc).
- [ ] Remove the GL renderer and the interop. Linux tested in the CachyOS VM.

## Risks and open questions

- Does our scene match what the game draws? LODs, culling by the game, special effects
  drawn by code instead of models. The debug view in step 2 decides this early.
- The 3D picture stays the game's raster for a long time, and RT effects read our scene.
  Where both differ, shadows and reflections show things that are not drawn, or miss things.
- Threads: Vulkan only on the main thread, as GL now (`window.rs`).
- Shader toolchain: naga (pure Rust, GLSL limits) or shaderc (C++ build).
