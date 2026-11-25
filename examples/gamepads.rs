//! List controllers visible to gilrs without starting the game.
fn main() {
    let mut gilrs = match gilrs::Gilrs::new() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("Could not initialize gilrs: {e}");
            return;
        }
    };
    for _ in 0..20 {
        while gilrs.next_event().is_some() {}
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    #[cfg(windows)]
    {
        // poll() starts the background worker; its first snapshot is necessarily empty.
        fcop::platform::dualsense_win::poll();
        std::thread::sleep(std::time::Duration::from_millis(250));
        for (id, pad) in fcop::platform::dualsense_win::poll() {
            println!("Windows joystick fallback: id={id} sticks={:?} buttons={:#06x} start={}", pad.stick, pad.buttons(), pad.start_pressed());
        }
        if std::env::args().any(|arg| arg == "--watch") {
            println!("Watching for 10 seconds: press Cross and move the left stick and D-pad.");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let mut previous = String::new();
            while std::time::Instant::now() < deadline {
                let snapshot = fcop::platform::dualsense_win::poll();
                let state = snapshot.iter().map(|(id, pad)| {
                    let sticks = pad.stick.map(|v| if v.abs() < 0.2 { 0.0 } else { (v * 10.0).round() / 10.0 });
                    let raw = fcop::platform::dualsense_win::diagnostic_raw(*id);
                    format!("id={id} sticks={sticks:?} buttons={:#06x} start={} raw={raw:?}", pad.buttons(), pad.start_pressed())
                }).collect::<Vec<_>>().join("; ");
                if state != previous {
                    println!("{}", if state.is_empty() { "No fallback device" } else { &state });
                    previous = state;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
    for (id, pad) in gilrs.gamepads() {
        println!(
            "id={} name={:?} vendor={:?} product={:?} mapping={:?} ff={}",
            usize::from(id), pad.name(), pad.vendor_id(), pad.product_id(),
            pad.mapping_source(), pad.is_ff_supported()
        );
    }
}
