#![allow(unused_imports)]
#![allow(clippy::missing_errors_doc)]

use std::{
    cell::Cell, future::Future, process::ExitCode, rc::Weak as WeakRc, sync::Weak as WeakArc,
};

use async_executor::{Executor, Task};
use mlua::prelude::*;
use tracing::trace;

use crate::{
    exit::Exit,
    queue::{DeferredThreadQueue, FuturesQueue, SpawnedThreadQueue},
    result_map::ThreadResultMap,
    runtime::Runtime,
    thread_id::ThreadId,
};

/**
    Trait for any struct that can be turned into an [`LuaThread`]
    and passed to the runtime, implemented for the following types:

    - Lua threads ([`LuaThread`])
    - Lua functions ([`LuaFunction`])
    - Lua chunks ([`LuaChunk`])
*/
pub trait IntoLuaThread<'lua> {
    /**
        Converts the value into a Lua thread.

        # Errors

        Errors when out of memory.
    */
    fn into_lua_thread(self, lua: &'lua Lua) -> LuaResult<LuaThread<'lua>>;
}

impl<'lua> IntoLuaThread<'lua> for LuaThread<'lua> {
    fn into_lua_thread(self, _: &'lua Lua) -> LuaResult<LuaThread<'lua>> {
        Ok(self)
    }
}

impl<'lua> IntoLuaThread<'lua> for LuaFunction<'lua> {
    fn into_lua_thread(self, lua: &'lua Lua) -> LuaResult<LuaThread<'lua>> {
        lua.create_thread(self)
    }
}

impl<'lua> IntoLuaThread<'lua> for LuaChunk<'lua, '_> {
    fn into_lua_thread(self, lua: &'lua Lua) -> LuaResult<LuaThread<'lua>> {
        lua.create_thread(self.into_function()?)
    }
}

impl<'lua, T> IntoLuaThread<'lua> for &T
where
    T: IntoLuaThread<'lua> + Clone,
{
    fn into_lua_thread(self, lua: &'lua Lua) -> LuaResult<LuaThread<'lua>> {
        self.clone().into_lua_thread(lua)
    }
}

/**
    Trait for interacting with the current [`Runtime`].

    Provides extra methods on the [`Lua`] struct for:

    - Setting the exit code and forcibly stopping the runtime
    - Pushing (spawning) and deferring (pushing to the back) lua threads
    - Tracking and getting the result of lua threads
*/
pub trait LuaRuntimeExt<'lua> {
    /**
        Sets the exit code of the current runtime.

        See [`Runtime::set_exit_code`] for more information.

        # Panics

        Panics if called outside of a running [`Runtime`].
    */
    fn set_exit_code(&self, code: ExitCode);

    /**
        Pushes (spawns) a lua thread to the **front** of the current runtime.

        See [`Runtime::push_thread_front`] for more information.

        # Panics

        Panics if called outside of a running [`Runtime`].
    */
    fn push_thread_front(
        &'lua self,
        thread: impl IntoLuaThread<'lua>,
        args: impl IntoLuaMulti<'lua>,
    ) -> LuaResult<ThreadId>;

    /**
        Pushes (defers) a lua thread to the **back** of the current runtime.

        See [`Runtime::push_thread_back`] for more information.

        # Panics

        Panics if called outside of a running [`Runtime`].
    */
    fn push_thread_back(
        &'lua self,
        thread: impl IntoLuaThread<'lua>,
        args: impl IntoLuaMulti<'lua>,
    ) -> LuaResult<ThreadId>;

    /**
        Registers the given thread to be tracked within the current runtime.

        Must be called before waiting for a thread to complete or getting its result.
    */
    fn track_thread(&'lua self, id: ThreadId);

    /**
        Gets the result of the given thread.

        See [`Runtime::get_thread_result`] for more information.

        # Panics

        Panics if called outside of a running [`Runtime`].
    */
    fn get_thread_result(&'lua self, id: ThreadId) -> Option<LuaResult<LuaMultiValue<'lua>>>;

    /**
        Waits for the given thread to complete.

        See [`Runtime::wait_for_thread`] for more information.

        # Panics

        Panics if called outside of a running [`Runtime`].
    */
    fn wait_for_thread(&'lua self, id: ThreadId) -> impl Future<Output = ()>;
}

/**
    Trait for interacting with the [`Executor`] for the current [`Runtime`].

    Provides extra methods on the [`Lua`] struct for:

    - Spawning thread-local (`!Send`) futures on the current executor
    - Spawning background (`Send`) futures on the current executor
    - Spawning blocking tasks on a separate thread pool
*/
pub trait LuaSpawnExt<'lua> {
    /**
        Spawns the given future on the current executor and returns its [`Task`].

        # Panics

        Panics if called outside of a running [`Runtime`].

        # Example usage

        ```rust
        use async_io::block_on;

        use mlua::prelude::*;
        use mlua_luau_runtime::*;

        fn main() -> LuaResult<()> {
            let lua = Lua::new();

            lua.globals().set(
                "spawnBackgroundTask",
                lua.create_async_function(|lua, ()| async move {
                    lua.spawn(async move {
                        println!("Hello from background task!");
                    }).await;
                    Ok(())
                })?
            )?;

            let rt = Runtime::new(&lua);
            rt.push_thread_front(lua.load("spawnBackgroundTask()"), ());
            block_on(rt.run());

            Ok(())
        }
        ```

        [`Runtime`]: crate::Runtime
    */
    fn spawn<F, T>(&self, fut: F) -> Task<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static;

    /**
        Spawns the given thread-local future on the current executor.

        Note that this future will run detached and always to completion,
        preventing the [`Runtime`] was spawned on from completing until done.

        # Panics

        Panics if called outside of a running [`Runtime`].

        # Example usage

        ```rust
        use async_io::block_on;

        use mlua::prelude::*;
        use mlua_luau_runtime::*;

        fn main() -> LuaResult<()> {
            let lua = Lua::new();

            lua.globals().set(
                "spawnLocalTask",
                lua.create_async_function(|lua, ()| async move {
                    lua.spawn_local(async move {
                        println!("Hello from local task!");
                    });
                    Ok(())
                })?
            )?;

            let rt = Runtime::new(&lua);
            rt.push_thread_front(lua.load("spawnLocalTask()"), ());
            block_on(rt.run());

            Ok(())
        }
        ```
    */
    fn spawn_local<F>(&self, fut: F)
    where
        F: Future<Output = ()> + 'static;

    /**
        Spawns the given blocking function and returns its [`Task`].

        This function will run on a separate thread pool and not block the current executor.

        # Panics

        Panics if called outside of a running [`Runtime`].

        # Example usage

        ```rust
        use async_io::block_on;

        use mlua::prelude::*;
        use mlua_luau_runtime::*;

        fn main() -> LuaResult<()> {
            let lua = Lua::new();

            lua.globals().set(
                "spawnBlockingTask",
                lua.create_async_function(|lua, ()| async move {
                    lua.spawn_blocking(|| {
                        println!("Hello from blocking task!");
                    }).await;
                    Ok(())
                })?
            )?;

            let rt = Runtime::new(&lua);
            rt.push_thread_front(lua.load("spawnBlockingTask()"), ());
            block_on(rt.run());

            Ok(())
        }
        ```
    */
    fn spawn_blocking<F, T>(&self, f: F) -> Task<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static;
}

impl<'lua> LuaRuntimeExt<'lua> for Lua {
    fn set_exit_code(&self, code: ExitCode) {
        let exit = self
            .app_data_ref::<Exit>()
            .expect("exit code can only be set within a runtime");
        exit.set(code);
    }

    fn push_thread_front(
        &'lua self,
        thread: impl IntoLuaThread<'lua>,
        args: impl IntoLuaMulti<'lua>,
    ) -> LuaResult<ThreadId> {
        let queue = self
            .app_data_ref::<SpawnedThreadQueue>()
            .expect("lua threads can only be pushed within a runtime");
        queue.push_item(self, thread, args)
    }

    fn push_thread_back(
        &'lua self,
        thread: impl IntoLuaThread<'lua>,
        args: impl IntoLuaMulti<'lua>,
    ) -> LuaResult<ThreadId> {
        let queue = self
            .app_data_ref::<DeferredThreadQueue>()
            .expect("lua threads can only be pushed within a runtime");
        queue.push_item(self, thread, args)
    }

    fn track_thread(&'lua self, id: ThreadId) {
        let map = self
            .app_data_ref::<ThreadResultMap>()
            .expect("lua threads can only be tracked within a runtime");
        map.track(id);
    }

    fn get_thread_result(&'lua self, id: ThreadId) -> Option<LuaResult<LuaMultiValue<'lua>>> {
        let map = self
            .app_data_ref::<ThreadResultMap>()
            .expect("lua threads results can only be retrieved within a runtime");
        map.remove(id).map(|r| r.value(self))
    }

    fn wait_for_thread(&'lua self, id: ThreadId) -> impl Future<Output = ()> {
        let map = self
            .app_data_ref::<ThreadResultMap>()
            .expect("lua threads results can only be retrieved within a runtime");
        async move { map.listen(id).await }
    }
}

impl<'lua> LuaSpawnExt<'lua> for Lua {
    fn spawn<F, T>(&self, fut: F) -> Task<T>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let exec = self
            .app_data_ref::<WeakArc<Executor>>()
            .expect("tasks can only be spawned within a runtime")
            .upgrade()
            .expect("executor was dropped");
        trace!("spawning future on executor");
        exec.spawn(fut)
    }

    fn spawn_local<F>(&self, fut: F)
    where
        F: Future<Output = ()> + 'static,
    {
        let queue = self
            .app_data_ref::<WeakRc<FuturesQueue>>()
            .expect("tasks can only be spawned within a runtime")
            .upgrade()
            .expect("executor was dropped");
        trace!("spawning local task on executor");
        queue.push_item(fut);
    }

    fn spawn_blocking<F, T>(&self, f: F) -> Task<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let exec = self
            .app_data_ref::<WeakArc<Executor>>()
            .expect("tasks can only be spawned within a runtime")
            .upgrade()
            .expect("executor was dropped");
        trace!("spawning blocking task on executor");
        exec.spawn(blocking::unblock(f))
    }
}
