//! Native panels must return to winit before waiting for user input.
//! Poll a sheet future once per frame; never block_on it or call runModal.
use bevy::prelude::*;
use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Wake, Waker},
};

type DialogFuture = Pin<Box<dyn Future<Output = Option<PathBuf>> + Send>>;
type Completion = Box<dyn FnOnce(&mut World, Option<PathBuf>) + Send>;
#[derive(Resource)]
struct PendingDialog(Mutex<(DialogFuture, Option<Completion>)>);

pub(crate) fn request(
    world: &mut World,
    future: impl Future<Output = Option<PathBuf>> + Send + 'static,
    completion: impl FnOnce(&mut World, Option<PathBuf>) + Send + 'static,
) -> Result<(), String> {
    if world.contains_resource::<PendingDialog>() {
        return Err("A file dialog is already open".into());
    }
    world.insert_resource(PendingDialog(Mutex::new((
        Box::pin(future),
        Some(Box::new(completion)),
    ))));
    // The poll system may already have run in this frame.
    if let Some(proxy) = world.get_resource::<bevy::winit::EventLoopProxyWrapper>() {
        let _ = proxy.send_event(bevy::winit::WinitUserEvent::WakeUp);
    }
    Ok(())
}

pub(crate) fn poll_native_dialog(world: &mut World) {
    let Some(dialog) = world.get_resource::<PendingDialog>() else {
        return;
    };
    let waker = world
        .get_resource::<bevy::winit::EventLoopProxyWrapper>()
        .map(|proxy| Waker::from(Arc::new(DialogWake((**proxy).clone()))));
    let result = {
        let mut state = dialog.0.lock().unwrap();
        let mut context = Context::from_waker(waker.as_ref().unwrap_or(Waker::noop()));
        match state.0.as_mut().poll(&mut context) {
            Poll::Pending => None,
            Poll::Ready(path) => Some((state.1.take().unwrap(), path)),
        }
    };
    if let Some((completion, path)) = result {
        world.remove_resource::<PendingDialog>();
        completion(world, path);
    }
}

// Desktop apps sleep between input events. Completion must wake the outer
// event loop, not synchronously reenter the current callback.
struct DialogWake(bevy::winit::EventLoopProxy<bevy::winit::WinitUserEvent>);
impl Wake for DialogWake {
    fn wake(self: Arc<Self>) {
        let _ = self.0.send_event(bevy::winit::WinitUserEvent::WakeUp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    #[test]
    fn pending_dialog_returns_to_schedule_and_rejects_duplicate() {
        let mut world = World::new();
        let ready = Arc::new(AtomicBool::new(false));
        let flag = ready.clone();
        request(
            &mut world,
            std::future::poll_fn(move |_| {
                if flag.load(Ordering::Relaxed) {
                    Poll::Ready(Some(PathBuf::from("chosen.talos3d")))
                } else {
                    Poll::Pending
                }
            }),
            |world, path| {
                world.insert_resource(Chosen(path));
            },
        )
        .unwrap();
        poll_native_dialog(&mut world);
        assert!(!world.contains_resource::<Chosen>());
        assert!(request(&mut world, async { None }, |_, _| {}).is_err());
        ready.store(true, Ordering::Relaxed);
        poll_native_dialog(&mut world);
        assert_eq!(
            world.resource::<Chosen>().0,
            Some(PathBuf::from("chosen.talos3d"))
        );
        assert!(!world.contains_resource::<PendingDialog>());
        poll_native_dialog(&mut world); // callback runs only once
    }
    #[derive(Resource)]
    struct Chosen(Option<PathBuf>);
    #[test]
    fn cancellation_is_delivered_and_releases_dialog_slot() {
        let mut world = World::new();
        request(&mut world, async { None }, |world, path| {
            world.insert_resource(Chosen(path));
        })
        .unwrap();
        poll_native_dialog(&mut world);
        assert!(world.resource::<Chosen>().0.is_none());
        assert!(request(&mut world, async { None }, |_, _| {}).is_ok());
    }
}
