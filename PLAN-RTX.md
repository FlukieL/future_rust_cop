# Plan — Vulkan, DLSS, RTX raytracing

Started 2026-11-21. Experimental, kept apart from PLAN.md. The main plan stays as it is.

Goal (the user's): modernize the renderer for RTX raytracing and DLSS. DLSS 5 needs no
work of its own: managers (NVIDIA App override, DLL swappers) hook into an existing DLSS
integration. Our job is a clean standard integration that they can find.

Hardware here: RTX 4090 (Ada). It supports DLSS Super Resolution, DLAA, Frame Generation
(2x), Ray Reconstruction and Reflex. It has no Multi Frame Generation, which needs RTX 50.

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
      with GL. ~13% of the game thread waits inside nvoglv64 (a GL call per picture, likely
      glWaitSemaphoreEXT on the last copy). Next try: a ring of 2-3 shared images, so GL never
      waits for the copy of the picture before.
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
- [ ] Instances: who is drawn, with which model and which transform.
      - Actors: list 0x50e428, count [0x50e420], matrix +0x2c, position +0x50 (`fps.rs`).
      - The robot's side weapons and lights: data [+0xac] at +0x178, +0x1c4, +0x210.
      - Static objects: grid [0x4a13fc] (`shadows.rs` step 2).
      - Understand model_draw 0x4280a0 (its arguments: model, matrix, position) and hook it.
        That gives every drawn model with its transform in one place.
      - The whole level is needed, not only the visible part: shadows and reflections come
        from things off screen too. The ring of `shadows.rs` shows how to reach them.
- [ ] Each instance gets a stable key (actor or object address plus model), so its last
      transform is known. Mind that a destroyed actor's memory may be reused (`fps.rs`).
- [ ] Materials: texture page and UVs per face from `3DTL`, pages from `Cbmp` (and HD
      replacements, `hd.rs`), chroma key as alpha test. Colour byte and flags per face.
- [ ] TLAS built per picture. Pictures drawn in between (`fps.rs`) use the interpolated
      transforms, so the scene must be read after `fps.rs` has placed things.
- [ ] Debug view `FCOP_GBUFFER=scene`: our scene rasterized from the game's camera, laid
      over the game's picture, plus a depth difference to `ao::depths`. Both must agree
      before anything builds on the scene.

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

- [ ] Sun shadows by ray query: per pixel the world position from the depth, one ray to the
      sun against the TLAS. Replaces the shadow map and its ring. Keyed texels by an
      any-hit / alpha test (or opacity micromaps).
- [ ] Shadows of the dynamic lights (`lights.rs`) by rays instead of the screen ray march.
- [ ] RT ambient occlusion instead of SSAO.
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
