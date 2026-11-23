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

Since (2026-11-22, pushed): RT ambient occlusion (step 5, done: 4 hemisphere rays a pixel,
blurred along the surface, rays turned by world cell: no wandering), shadow rays start off the
surface along its normal (no flicker on ground seen at a flat angle), the debug menu
(Ctrl+Shift+D in `--features cheats` builds, `debug.rs`), and step 3's first part: the camera's
motion vectors (`motion.rs`, `FCOP_GBUFFER=motion` / `reproject`).

Next (where to go on after a break):
- Step 3 done but the check below (moving things' motion, jitter `jitter.rs`).
- Step 4 (2026-11-22, pushed): licence checked, the SDK files by download (`sdk.rs`), Streamline
  started with `FCOP_DLSS=1` (`streamline.rs`), DLSS reported supported. To test: download once
  with `cargo test --release --lib sdk -- --ignored` (lands in target/release/deps/game_files/DLSS:
  move it to target/release/game_files/DLSS), then run with `FCOP_DLSS=1`.
- DLSS pass, first part (2026-11-22, not committed): DLAA at the target's size, per view in
  `glide::post_3d` after AO/lights/shadows/heat, before bloom and tone mapping (`Vk::dlss`):
  GL blits the picture (resolved) and draws depth (8 / z, inverted) and the motion vectors into
  shared images (RGBA16F, R32F, RG16F), signals; Vulkan: `streamline::evaluate`
  (slGetNewFrameToken with the picture's number, slSetConstants, slDLSSSetOptions,
  slSetTagForFrame, slEvaluateFeature) into an RGBA16F output, GL waits and draws it back over
  the view. Jitter and motion vectors are on while DLSS runs (`vk::dlss_ready`). Picture right
  (upright, edges smooth). ~190 fps against ~430 without (first mission start, 1280x960): ~2.9 ms,
  far more than DLAA should cost; measure with timestamps (the synchronous hand-over?). The
  picture stands still (the user: jitter right). The privateData feature is on with Streamline
  (its vkCreatePrivateDataSlot; validation clean). Open: MSAA still on (should be off with DLSS),
  split screen untested.
- DLSS's cost measured (2026-11-23, `FCOP_RT_TIME=1` now also logs DLSS: GPU timestamps around
  the evaluation, CPU times of the hand-over). First mission start, 1280x960, RT shadows + RT AO,
  MSAA off. The GPU is the limit here (`nvidia-smi dmon`: SM ~95% with and without DLSS):
  Vulkan alone ~525 fps (1.90 ms); Streamline's interposer loaded, no DLSS pass ~458 (2.18);
  the pass without `slEvaluateFeature` ~395 (2.53: copies and the GL -> Vulkan -> GL round
  trip); with it ~340 (2.94; the evaluation ~305 µs GPU). Motion vectors and jitter cost
  nothing measurable. The CPU waited ~0.6 ms a view for the last DLSS's fence: two recordings
  in turn now (wait ~2 µs), fps the same (GPU bound). glSignalSemaphore ~165 µs CPU,
  glWaitSemaphore ~25 µs. The earlier ~190 fps was a run-to-run state (seen once again, ~190
  with the same build). Conclusion: render below the output size is the real gain (raster, RT
  and AO scale with the pixels); the round trip's ~0.35 ms goes away only with step 6.
- Next: render size below the output (the 3D picture into a smaller framebuffer, DLSS to the
  view's box, HUD at full size), the DLSS setting. The plan as it was: GL draws the 3D picture at the render size; colour (before the HUD),
  depth, motion vectors into images shared with Vulkan (as the sun's rays: synchronous,
  semaphores both ways); Streamline: `slGetNewFrameToken`, `slSetConstants` (camera matrices,
  jitter `jitter::now`, mvecScale 1/size), `slDLSSSetOptions` (mode, output size),
  `slSetTagForFrame` (colour in, depth, mvec, colour out), `slEvaluateFeature(DLSS)` on a Vulkan
  command buffer; the output back to GL at the window's size, then bloom, tone mapping, HUD.
  Declarations: Streamline's headers (MIT) in github.com/NVIDIA-RTX/Streamline include/
  (sl_core_types.h: Constants, ResourceTag, Resource; sl_dlss.h: DLSSOptions, DLSSMode,
  slDLSSGetOptimalSettings through `slGetFeatureFunction`); docs/ProgrammingGuideDLSS.md. Check
  the "Hook sl.common:Vulkan:... NOT supported" warnings then. After it: the download dialog,
  the DLSS setting in Advanced Graphics, DLSS-G, Reflex.
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

- [x] The camera's motion (2026-11-22, `motion.rs`): per pixel from the depth into the last
      picture's view (true inverse of the camera matrix: the game's are not orthonormal);
      `FCOP_GBUFFER=reproject` black when walking and turning but for moving things.
- [x] Moving things' motion per pixel (2026-11-22): the drawn models and parts whose world
      transform changed, rasterized with this picture's and the last picture's transforms and
      cameras, where their depth matches the game's. Elsewhere (effects, sprites) only the
      camera's motion from the depth. In `reproject` the walking, turning robot is dark but
      for its edges (disocclusion), its light bar's glows and the shading that turns with it.
      Not followed: vertex animation (models with frames).
- [x] Vectors between shown pictures, not between game frames (pictures come between frames):
      the interpolated camera of each picture.
- [x] Jitter (2026-11-22, `jitter.rs`, `FCOP_JITTER=1`): a Halton (2, 3) subpixel offset, 8
      points, one per picture shown. Not in the projection hooks as planned (they give whole
      2560x1920 units, coarser than a pixel at high resolutions) but in Glide's vertex shader
      for the 3D picture's primitives; the HUD stays still. Motion vectors without jitter.
      DLSS will want the phase count from the scale (8 * (display / render)^2).
- [ ] Check: `FCOP_GBUFFER=motion` (colour by direction). A reprojected last picture must
      lie on the new one when walking and turning.

## Step 4: DLSS through Streamline

- [ ] The SDK's files by download, never in the repository or the release (decided
      2026-11-22): when the player chooses DLSS in Advanced Graphics and it is missing,
      "Download..." shows NVIDIA's licence (`nvngx_dlss.license.txt`) with Accept / Cancel,
      then fetches only the needed entries of NVIDIA's official Streamline release zip on
      GitHub (276 MB; HTTP range requests: its central directory, then each entry, ~37 MB
      packed): `bin/x64/` sl.interposer, sl.common, sl.dlss, sl.dlss_g, sl.reflex, sl.pcl,
      NvLowLatencyVk.dll, nvngx_dlss.dll, nvngx_dlssg.dll, the licence texts and the root
      `license.txt` (Streamline's MIT). Inflated, each checked against a SHA-256 in our code
      (version and hashes fixed, updated by hand), into `game_files/DLSS/` (kept by `setup`).
      On its own thread with progress. Never at start, only on the player's choice. A DLL
      changed later by DLSS Swapper / the NVIDIA App is no error (DLSS 5 by managers).
      Streamline is Windows only: DLSS too.
- [x] The download (2026-11-22, `sdk.rs`; its dialog in Advanced Graphics still open).
- [ ] Streamline started (2026-11-22, `streamline.rs`, `FCOP_DLSS=1`): interposer as the Vulkan
      loader, `slInit`, "DLSS supported on NVIDIA GeForce RTX 4090", the game runs as before.
      Next: the DLSS pass itself (render size, tags, constants, evaluate), then DLSS-G, Reflex.
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
- [x] Licence checked (2026-11-22, Streamline v2.14.16; not legal advice):
      - Streamline itself (headers, `sl.interposer.dll`, `sl.*.dll` plugins) is MIT
        (`license.txt`): may be shipped, its notice beside it.
      - The DLSS DLLs (`nvngx_dlss.dll`, `nvngx_dlssg.dll`) fall under the NVIDIA RTX SDKs
        licence (`external/ngx-sdk/license.txt`), Reflex under NVIDIA's SDK licence: shipping
        allowed only inside an application with functionality of its own, under terms at least
        as protective as NVIDIA's, never made subject to an open source licence (our MIT must
        not cover them), and then: NVIDIA's marks on splash screen, about box and credits (marks
        only after NVIDIA's written approval of a sample), notice to NVIDIA before a commercial
        release, NVIDIA GPUs only, NVIDIA may disable a broken integration. Community opinion
        (Nexus Mods) says the DLLs may not be passed on at all; Unigine ships them with their
        licence text. Questions: nvidia-rtx-license-questions@nvidia.com.
      - Never in the repository. Open: ship them in the release, or the player puts
        `nvngx_dlss.dll` beside fcop.exe (the user decides).

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
