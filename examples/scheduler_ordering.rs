use std::time::{Duration, Instant};

use mlua::prelude::*;
use smol::Timer;
use smol_mlua::Runtime;

const MAIN_SCRIPT: &str = include_str!("./lua/scheduler_ordering.luau");

pub fn main() -> LuaResult<()> {
    // Set up persistent lua environment
    let lua = Lua::new();
    let rt = Runtime::new(&lua)?;

    lua.globals().set("spawn", rt.create_spawn_function()?)?;
    lua.globals().set("defer", rt.create_defer_function()?)?;
    lua.globals().set(
        "sleep",
        lua.create_async_function(|_, duration: Option<f64>| async move {
            let duration = duration.unwrap_or_default().max(1.0 / 250.0);
            let before = Instant::now();
            let after = Timer::after(Duration::from_secs_f64(duration)).await;
            Ok((after - before).as_secs_f64())
        })?,
    )?;

    // Load the main script into a runtime and run it until completion
    let main = lua.load(MAIN_SCRIPT);
    rt.spawn_thread(main, ())?;
    rt.run_blocking();

    Ok(())
}

#[test]
fn test_scheduler_ordering() -> LuaResult<()> {
    main()
}
