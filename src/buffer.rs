/*
 * Copyright (C) 2024 Open Source Robotics Foundation
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 *
*/

use bevy_ecs::{
    change_detection::Mut,
    prelude::{Commands, Entity, Query, World},
    query::QueryEntityError,
    system::{SystemParam, SystemState},
};

use std::{ops::RangeBounds, sync::Arc};

use thiserror::Error as ThisError;

use crate::{
    Builder, Chain, GateState, InputSlot, NotifyBufferUpdate, OnNewBufferValue, UnusedTarget,
};

mod any_buffer;
pub use any_buffer::*;

mod buffer_access_lifecycle;
pub use buffer_access_lifecycle::BufferKeyLifecycle;
pub(crate) use buffer_access_lifecycle::*;

mod buffer_key_builder;
pub use buffer_key_builder::*;

mod buffer_gate;
pub use buffer_gate::*;

mod buffer_map;
pub use buffer_map::*;

mod buffer_storage;
pub(crate) use buffer_storage::*;

mod buffering;
pub use buffering::*;

mod bufferable;
pub use bufferable::*;

mod manage_buffer;
pub use manage_buffer::*;

#[cfg(feature = "diagram")]
mod json_buffer;
#[cfg(feature = "diagram")]
pub use json_buffer::*;

/// A buffer is a special type of node within a workflow that is able to store
/// and release data. When a session is finished, the buffered data from the
/// session will be automatically cleared.
pub struct Buffer<T> {
    pub(crate) location: BufferLocation,
    pub(crate) _ignore: std::marker::PhantomData<fn(T)>,
}

impl<T> Buffer<T> {
    /// Get a unit `()` trigger output each time a new value is added to the buffer.
    pub fn on_new_value<'w, 's, 'a, 'b>(
        &self,
        builder: &'b mut Builder<'w, 's, 'a>,
    ) -> Chain<'w, 's, 'a, 'b, ()> {
        assert_eq!(self.scope(), builder.scope());
        let target = builder.commands.spawn(UnusedTarget).id();
        builder
            .commands
            .add(OnNewBufferValue::new(self.id(), target));
        Chain::new(target, builder)
    }

    /// Specify that you want to pull from this Buffer by cloning. This can be
    /// used by operations like join to tell them that they should clone from
    /// the buffer instead of consuming from it.
    pub fn by_cloning(self) -> CloneFromBuffer<T>
    where
        T: Clone,
    {
        CloneFromBuffer {
            location: self.location,
            _ignore: Default::default(),
        }
    }

    /// Get an input slot for this buffer.
    pub fn input_slot(self) -> InputSlot<T> {
        InputSlot::new(self.scope(), self.id())
    }

    /// Get the entity ID of the buffer.
    pub fn id(&self) -> Entity {
        self.location.source
    }

    /// Get the ID of the workflow that the buffer is associated with.
    pub fn scope(&self) -> Entity {
        self.location.scope
    }

    /// Get general information about the buffer.
    pub fn location(&self) -> BufferLocation {
        self.location
    }
}

impl<T> Clone for Buffer<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Buffer<T> {}

/// The general identifying information for a buffer to locate it within the
/// world. This does not indicate anything about the type of messages that the
/// buffer can contain.
#[derive(Clone, Copy, Debug)]
pub struct BufferLocation {
    /// The entity ID of the buffer.
    pub scope: Entity,
    /// The ID of the workflow that the buffer is associated with.
    pub source: Entity,
}

#[derive(Clone)]
pub struct CloneFromBuffer<T: Clone> {
    pub(crate) location: BufferLocation,
    pub(crate) _ignore: std::marker::PhantomData<fn(T)>,
}

//
impl<T: Clone> Copy for CloneFromBuffer<T> {}

impl<T: Clone> CloneFromBuffer<T> {
    /// Get the entity ID of the buffer.
    pub fn id(&self) -> Entity {
        self.location.source
    }

    /// Get the ID of the workflow that the buffer is associated with.
    pub fn scope(&self) -> Entity {
        self.location.scope
    }

    /// Get general information about the buffer.
    pub fn location(&self) -> BufferLocation {
        self.location
    }
}

impl<T: Clone> From<CloneFromBuffer<T>> for Buffer<T> {
    fn from(value: CloneFromBuffer<T>) -> Self {
        Buffer {
            location: value.location,
            _ignore: Default::default(),
        }
    }
}

/// Settings to describe the behavior of a buffer.
#[cfg_attr(
    feature = "diagram",
    derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema),
    serde(rename_all = "snake_case")
)]
#[derive(Default, Clone, Copy, Debug)]
pub struct BufferSettings {
    retention: RetentionPolicy,
}

impl BufferSettings {
    /// Define new buffer settings
    pub fn new(retention: RetentionPolicy) -> Self {
        Self { retention }
    }

    /// Create `BufferSettings` with a retention policy of [`RetentionPolicy::KeepLast`]`(n)`.
    pub fn keep_last(n: usize) -> Self {
        Self::new(RetentionPolicy::KeepLast(n))
    }

    /// Create `BufferSettings` with a retention policy of [`RetentionPolicy::KeepFirst`]`(n)`.
    pub fn keep_first(n: usize) -> Self {
        Self::new(RetentionPolicy::KeepFirst(n))
    }

    /// Create `BufferSettings` with a retention policy of [`RetentionPolicy::KeepAll`].
    pub fn keep_all() -> Self {
        Self::new(RetentionPolicy::KeepAll)
    }

    /// Get the retention policy for the buffer.
    pub fn retention(&self) -> RetentionPolicy {
        self.retention
    }

    /// Modify the retention policy for the buffer.
    pub fn retention_mut(&mut self) -> &mut RetentionPolicy {
        &mut self.retention
    }
}

/// Describe how data within a buffer gets retained. Most mechanisms that pull
/// data from a buffer will remove the oldest item in the buffer, so this policy
/// is for dealing with situations where items are being stored faster than they
/// are being pulled.
///
/// The default value is KeepLast(1).
#[cfg_attr(
    feature = "diagram",
    derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema),
    serde(rename_all = "snake_case")
)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum RetentionPolicy {
    /// Keep the last N items that were stored into the buffer. Once the limit
    /// is reached, the oldest item will be removed any time a new item arrives.
    KeepLast(usize),
    /// Keep the first N items that are stored into the buffer. Once the limit
    /// is reached, any new item that arrives will be discarded.
    KeepFirst(usize),
    /// Do not limit how many items can be stored in the buffer.
    KeepAll,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self::KeepLast(1)
    }
}

/// This key can unlock access to the contents of a buffer by passing it into
/// [`BufferAccess`] or [`BufferAccessMut`].
///
/// To obtain a `BufferKey`, use [`Chain::with_access`][1], or [`listen`][2].
///
/// [1]: crate::Chain::with_access
/// [2]: crate::Accessible::listen
pub struct BufferKey<T> {
    tag: BufferKeyTag,
    _ignore: std::marker::PhantomData<fn(T)>,
}

impl<T> Clone for BufferKey<T> {
    fn clone(&self) -> Self {
        Self {
            tag: self.tag.clone(),
            _ignore: Default::default(),
        }
    }
}

impl<T> BufferKey<T> {
    /// The buffer ID of this key.
    pub fn buffer(&self) -> Entity {
        self.tag.buffer
    }

    /// The session that this key belongs to.
    pub fn session(&self) -> Entity {
        self.tag.session
    }

    pub fn tag(&self) -> &BufferKeyTag {
        &self.tag
    }
}

impl<T> BufferKeyLifecycle for BufferKey<T> {
    type TargetBuffer = Buffer<T>;

    fn create_key(buffer: &Self::TargetBuffer, builder: &BufferKeyBuilder) -> Self {
        BufferKey {
            tag: builder.make_tag(buffer.id()),
            _ignore: Default::default(),
        }
    }

    fn is_in_use(&self) -> bool {
        self.tag.is_in_use()
    }

    fn deep_clone(&self) -> Self {
        Self {
            tag: self.tag.deep_clone(),
            _ignore: Default::default(),
        }
    }
}

impl<T> std::fmt::Debug for BufferKey<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BufferKey")
            .field("message_type_name", &std::any::type_name::<T>())
            .field("tag", &self.tag)
            .finish()
    }
}

/// The identifying information for a buffer key. This does not indicate
/// anything about the type of messages that the buffer can contain.
#[derive(Clone)]
pub struct BufferKeyTag {
    pub buffer: Entity,
    pub session: Entity,
    pub accessor: Entity,
    pub lifecycle: Option<Arc<BufferAccessLifecycle>>,
}

impl BufferKeyTag {
    pub fn is_in_use(&self) -> bool {
        self.lifecycle.as_ref().is_some_and(|l| l.is_in_use())
    }

    pub fn deep_clone(&self) -> Self {
        let mut deep = self.clone();
        deep.lifecycle = self
            .lifecycle
            .as_ref()
            .map(|l| Arc::new(l.as_ref().clone()));
        deep
    }
}

impl std::fmt::Debug for BufferKeyTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BufferKeyTag")
            .field("buffer", &self.buffer)
            .field("session", &self.session)
            .field("accessor", &self.accessor)
            .field("in_use", &self.is_in_use())
            .finish()
    }
}

/// This system parameter lets you get read-only access to a buffer that exists
/// within a workflow. Use a [`BufferKey`] to unlock the access.
///
/// See [`BufferAccessMut`] for mutable access.
#[derive(SystemParam)]
pub struct BufferAccess<'w, 's, T>
where
    T: 'static + Send + Sync,
{
    query: Query<'w, 's, &'static BufferStorage<T>>,
}

impl<'w, 's, T: 'static + Send + Sync> BufferAccess<'w, 's, T> {
    pub fn get<'a>(&'a self, key: &BufferKey<T>) -> Result<BufferView<'a, T>, QueryEntityError> {
        let session = key.session();
        self.query
            .get(key.buffer())
            .map(|storage| BufferView { storage, session })
    }

    pub fn get_newest<'a>(&'a self, key: &BufferKey<T>) -> Option<&'a T> {
        self.get(key).ok().map(|view| view.newest()).flatten()
    }
}

/// This system parameter lets you get mutable access to a buffer that exists
/// within a workflow. Use a [`BufferKey`] to unlock the access.
///
/// See [`BufferAccess`] for read-only access.
#[derive(SystemParam)]
pub struct BufferAccessMut<'w, 's, T>
where
    T: 'static + Send + Sync,
{
    query: Query<'w, 's, &'static mut BufferStorage<T>>,
    commands: Commands<'w, 's>,
}

impl<'w, 's, T> BufferAccessMut<'w, 's, T>
where
    T: 'static + Send + Sync,
{
    pub fn get<'a>(&'a self, key: &BufferKey<T>) -> Result<BufferView<'a, T>, QueryEntityError> {
        let session = key.session();
        self.query
            .get(key.buffer())
            .map(|storage| BufferView { storage, session })
    }

    pub fn get_newest<'a>(&'a self, key: &BufferKey<T>) -> Option<&'a T> {
        self.get(key).ok().map(|view| view.newest()).flatten()
    }

    pub fn get_mut<'a>(
        &'a mut self,
        key: &BufferKey<T>,
    ) -> Result<BufferMut<'w, 's, 'a, T>, QueryEntityError> {
        let buffer = key.buffer();
        let session = key.session();
        let accessor = key.tag.accessor;
        self.query
            .get_mut(key.buffer())
            .map(|storage| BufferMut::new(storage, buffer, session, accessor, &mut self.commands))
    }
}

/// This trait allows [`World`] to give you access to any buffer using a [`BufferKey`]
pub trait BufferWorldAccess {
    /// Call this to get read-only access to a buffer from a [`World`].
    ///
    /// Alternatively you can use [`BufferAccess`] as a regular bevy system parameter,
    /// which does not need direct world access.
    fn buffer_view<T>(&self, key: &BufferKey<T>) -> Result<BufferView<'_, T>, BufferError>
    where
        T: 'static + Send + Sync;

    /// Call this to get read-only access to the gate of a buffer from a [`World`].
    fn buffer_gate_view(
        &self,
        key: impl Into<AnyBufferKey>,
    ) -> Result<BufferGateView<'_>, BufferError>;

    /// Call this to get mutable access to a buffer.
    ///
    /// Pass in a callback that will receive [`BufferMut`], allowing it to view
    /// and modify the contents of the buffer.
    fn buffer_mut<T, U>(
        &mut self,
        key: &BufferKey<T>,
        f: impl FnOnce(BufferMut<T>) -> U,
    ) -> Result<U, BufferError>
    where
        T: 'static + Send + Sync;

    /// Call this to get mutable access to the gate of a buffer.
    ///
    /// Pass in a callback that will receive [`BufferGateMut`], allowing it to
    /// view and modify the gate of the buffer.
    fn buffer_gate_mut<U>(
        &mut self,
        key: impl Into<AnyBufferKey>,
        f: impl FnOnce(BufferGateMut) -> U,
    ) -> Result<U, BufferError>;
}

impl BufferWorldAccess for World {
    fn buffer_view<T>(&self, key: &BufferKey<T>) -> Result<BufferView<'_, T>, BufferError>
    where
        T: 'static + Send + Sync,
    {
        let buffer_ref = self
            .get_entity(key.tag.buffer)
            .ok_or(BufferError::BufferMissing)?;
        let storage = buffer_ref
            .get::<BufferStorage<T>>()
            .ok_or(BufferError::BufferMissing)?;
        Ok(BufferView {
            storage,
            session: key.tag.session,
        })
    }

    fn buffer_gate_view(
        &self,
        key: impl Into<AnyBufferKey>,
    ) -> Result<BufferGateView<'_>, BufferError> {
        let key: AnyBufferKey = key.into();
        let buffer_ref = self
            .get_entity(key.tag.buffer)
            .ok_or(BufferError::BufferMissing)?;
        let gate = buffer_ref
            .get::<GateState>()
            .ok_or(BufferError::BufferMissing)?;
        Ok(BufferGateView {
            gate,
            session: key.tag.session,
        })
    }

    fn buffer_mut<T, U>(
        &mut self,
        key: &BufferKey<T>,
        f: impl FnOnce(BufferMut<T>) -> U,
    ) -> Result<U, BufferError>
    where
        T: 'static + Send + Sync,
    {
        let mut state = SystemState::<BufferAccessMut<T>>::new(self);
        let mut buffer_access_mut = state.get_mut(self);
        let buffer_mut = buffer_access_mut
            .get_mut(key)
            .map_err(|_| BufferError::BufferMissing)?;
        Ok(f(buffer_mut))
    }

    fn buffer_gate_mut<U>(
        &mut self,
        key: impl Into<AnyBufferKey>,
        f: impl FnOnce(BufferGateMut) -> U,
    ) -> Result<U, BufferError> {
        let mut state = SystemState::<BufferGateAccessMut>::new(self);
        let mut buffer_gate_access_mut = state.get_mut(self);
        let buffer_mut = buffer_gate_access_mut
            .get_mut(key)
            .map_err(|_| BufferError::BufferMissing)?;
        Ok(f(buffer_mut))
    }
}

/// Access to view a buffer that exists inside a workflow.
pub struct BufferView<'a, T>
where
    T: 'static + Send + Sync,
{
    storage: &'a BufferStorage<T>,
    session: Entity,
}

impl<'a, T> BufferView<'a, T>
where
    T: 'static + Send + Sync,
{
    /// Iterate over the contents in the buffer
    pub fn iter(&self) -> IterBufferView<'a, T> {
        self.storage.iter(self.session)
    }

    /// Borrow the oldest item in the buffer.
    pub fn oldest(&self) -> Option<&'a T> {
        self.storage.oldest(self.session)
    }

    /// Borrow the newest item in the buffer.
    pub fn newest(&self) -> Option<&'a T> {
        self.storage.newest(self.session)
    }

    /// Borrow an item from the buffer. Index 0 is the oldest item in the buffer
    /// with the highest index being the newest item in the buffer.
    pub fn get(&self, index: usize) -> Option<&'a T> {
        self.storage.get(self.session, index)
    }

    /// How many items are in the buffer?
    pub fn len(&self) -> usize {
        self.storage.count(self.session)
    }

    /// Check if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Access to mutate a buffer that exists inside a workflow.
pub struct BufferMut<'w, 's, 'a, T>
where
    T: 'static + Send + Sync,
{
    storage: Mut<'a, BufferStorage<T>>,
    buffer: Entity,
    session: Entity,
    accessor: Option<Entity>,
    commands: &'a mut Commands<'w, 's>,
    modified: bool,
}

impl<'w, 's, 'a, T> BufferMut<'w, 's, 'a, T>
where
    T: 'static + Send + Sync,
{
    /// When you make a modification using this `BufferMut`, anything listening
    /// to the buffer will be notified about the update. This can create
    /// unintentional infinite loops where a node in the workflow wakes itself
    /// up every time it runs because of a modification it makes to a buffer.
    ///
    /// By default this closed loop is disabled by keeping track of which
    /// listener created the key that's being used to modify the buffer, and
    /// then skipping that listener when notifying about the modification.
    ///
    /// In some cases a key can be used far downstream of the listener. In that
    /// case, there may be nodes downstream of the listener that do want to be
    /// woken up by the modification. Use this function to allow that closed
    /// loop to happen. It will be up to you to prevent the closed loop from
    /// being a problem.
    pub fn allow_closed_loops(mut self) -> Self {
        self.accessor = None;
        self
    }

    /// Iterate over the contents in the buffer.
    pub fn iter(&self) -> IterBufferView<'_, T> {
        self.storage.iter(self.session)
    }

    /// Look at the oldest item in the buffer.
    pub fn oldest(&self) -> Option<&T> {
        self.storage.oldest(self.session)
    }

    /// Look at the newest item in the buffer.
    pub fn newest(&self) -> Option<&T> {
        self.storage.newest(self.session)
    }

    /// Borrow an item from the buffer. Index 0 is the oldest item in the buffer
    /// with the highest index being the newest item in the buffer.
    pub fn get(&self, index: usize) -> Option<&T> {
        self.storage.get(self.session, index)
    }

    /// How many items are in the buffer?
    pub fn len(&self) -> usize {
        self.storage.count(self.session)
    }

    /// Check if the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Iterate over mutable borrows of the contents in the buffer.
    pub fn iter_mut(&mut self) -> IterBufferMut<'_, T> {
        self.modified = true;
        self.storage.iter_mut(self.session)
    }

    /// Modify the oldest item in the buffer.
    pub fn oldest_mut(&mut self) -> Option<&mut T> {
        self.modified = true;
        self.storage.oldest_mut(self.session)
    }

    /// Modify the newest item in the buffer.
    pub fn newest_mut(&mut self) -> Option<&mut T> {
        self.modified = true;
        self.storage.newest_mut(self.session)
    }

    /// Modify the newest item in the buffer or create a default-initialized
    /// item to modify if the buffer was empty.
    ///
    /// This may fail to provide a mutable borrow if the buffer was already
    /// expired or if the buffer capacity was zero.
    pub fn newest_mut_or_default(&mut self) -> Option<&mut T>
    where
        T: Default,
    {
        self.newest_mut_or_else(|| T::default())
    }

    /// Modify the newest item in the buffer or initialize an item if the
    /// buffer was empty.
    ///
    /// This may fail to provide a mutable borrow if the buffer was already
    /// expired or if the buffer capacity was zero.
    pub fn newest_mut_or_else(&mut self, f: impl FnOnce() -> T) -> Option<&mut T> {
        self.modified = true;
        self.storage.newest_mut_or_else(self.session, f)
    }

    /// Modify an item in the buffer. Index 0 is the oldest item in the buffer
    /// with the highest index being the newest item in the buffer.
    pub fn get_mut(&mut self, index: usize) -> Option<&mut T> {
        self.modified = true;
        self.storage.get_mut(self.session, index)
    }

    /// Drain items out of the buffer
    pub fn drain<R>(&mut self, range: R) -> DrainBuffer<'_, T>
    where
        R: RangeBounds<usize>,
    {
        self.modified = true;
        self.storage.drain(self.session, range)
    }

    /// Pull the oldest item from the buffer.
    pub fn pull(&mut self) -> Option<T> {
        self.modified = true;
        self.storage.pull(self.session)
    }

    /// Pull the item that was most recently put into the buffer (instead of
    /// the oldest, which is what [`Self::pull`] gives).
    pub fn pull_newest(&mut self) -> Option<T> {
        self.modified = true;
        self.storage.pull_newest(self.session)
    }

    /// Push a new value into the buffer. If the buffer is at its limit, this
    /// will return the value that needed to be removed.
    pub fn push(&mut self, value: T) -> Option<T> {
        self.modified = true;
        self.storage.push(self.session, value)
    }

    /// Push a value into the buffer as if it is the oldest value of the buffer.
    /// If the buffer is at its limit, this will return the value that needed to
    /// be removed.
    pub fn push_as_oldest(&mut self, value: T) -> Option<T> {
        self.modified = true;
        self.storage.push_as_oldest(self.session, value)
    }

    /// Trigger the listeners for this buffer to wake up even if nothing in the
    /// buffer has changed. This could be used for timers or timeout elements
    /// in a workflow.
    pub fn pulse(&mut self) {
        self.modified = true;
    }

    fn new(
        storage: Mut<'a, BufferStorage<T>>,
        buffer: Entity,
        session: Entity,
        accessor: Entity,
        commands: &'a mut Commands<'w, 's>,
    ) -> Self {
        Self {
            storage,
            buffer,
            session,
            accessor: Some(accessor),
            commands,
            modified: false,
        }
    }
}

impl<'w, 's, 'a, T> Drop for BufferMut<'w, 's, 'a, T>
where
    T: 'static + Send + Sync,
{
    fn drop(&mut self) {
        if self.modified {
            self.commands.add(NotifyBufferUpdate::new(
                self.buffer,
                self.session,
                self.accessor,
            ));
        }
    }
}

#[derive(ThisError, Debug, Clone)]
pub enum BufferError {
    #[error("The key was unable to identify a buffer")]
    BufferMissing,
}

#[cfg(test)]
mod tests {
    use crate::{prelude::*, testing::*, Gate};
    use std::future::Future;

    #[test]
    fn test_buffer_key_access() {
        let mut context = TestingContext::minimal_plugins();

        let add_buffers_by_pull_cb = add_buffers_by_pull.into_blocking_callback();
        let add_from_buffer_cb = add_from_buffer.into_blocking_callback();
        let multiply_buffers_by_copy_cb = multiply_buffers_by_copy.into_blocking_callback();

        let workflow = context.spawn_io_workflow(|scope: Scope<(f64, f64), f64>, builder| {
            scope
                .input
                .chain(builder)
                .unzip()
                .listen(builder)
                .then(multiply_buffers_by_copy_cb)
                .connect(scope.terminate);
        });

        let mut promise =
            context.command(|commands| commands.request((2.0, 3.0), workflow).take_response());

        context.run_with_conditions(&mut promise, Duration::from_secs(2));
        assert!(promise.take().available().is_some_and(|value| value == 6.0));
        assert!(context.no_unhandled_errors());

        let workflow = context.spawn_io_workflow(|scope: Scope<(f64, f64), f64>, builder| {
            scope
                .input
                .chain(builder)
                .unzip()
                .listen(builder)
                .then(add_buffers_by_pull_cb)
                .dispose_on_none()
                .connect(scope.terminate);
        });

        let mut promise =
            context.command(|commands| commands.request((4.0, 5.0), workflow).take_response());

        context.run_with_conditions(&mut promise, Duration::from_secs(2));
        assert!(promise.take().available().is_some_and(|value| value == 9.0));
        assert!(context.no_unhandled_errors());

        let workflow =
            context.spawn_io_workflow(|scope: Scope<(f64, f64), Result<f64, f64>>, builder| {
                let (branch_to_adder, branch_to_buffer) = scope.input.chain(builder).unzip();
                let buffer = builder.create_buffer::<f64>(BufferSettings::keep_first(10));
                builder.connect(branch_to_buffer, buffer.input_slot());

                let adder_node = branch_to_adder
                    .chain(builder)
                    .with_access(buffer)
                    .then_node(add_from_buffer_cb.clone());

                adder_node.output.chain(builder).fork_result(
                    // If the buffer had an item in it, we send it to another
                    // node that tries to pull a second time (we expect the
                    // buffer to be empty this second time) and then
                    // terminates.
                    |chain| {
                        chain
                            .with_access(buffer)
                            .then(add_from_buffer_cb.clone())
                            .connect(scope.terminate)
                    },
                    // If the buffer was empty, keep looping back until there
                    // is a value available.
                    |chain| chain.with_access(buffer).connect(adder_node.input),
                );
            });

        let mut promise =
            context.command(|commands| commands.request((2.0, 3.0), workflow).take_response());

        context.run_with_conditions(&mut promise, Duration::from_secs(2));
        assert!(promise
            .take()
            .available()
            .is_some_and(|value| value.is_err_and(|n| n == 5.0)));
        assert!(context.no_unhandled_errors());

        // Same as previous test, but using Builder::create_buffer_access instead
        let workflow = context.spawn_io_workflow(|scope, builder| {
            let (branch_to_adder, branch_to_buffer) = scope.input.chain(builder).unzip();
            let buffer = builder.create_buffer::<f64>(BufferSettings::keep_first(10));
            builder.connect(branch_to_buffer, buffer.input_slot());

            let access = builder.create_buffer_access(buffer);
            builder.connect(branch_to_adder, access.input);
            access
                .output
                .chain(builder)
                .then(add_from_buffer_cb.clone())
                .fork_result(
                    |ok| {
                        let (output, builder) = ok.unpack();
                        let second_access = builder.create_buffer_access(buffer);
                        builder.connect(output, second_access.input);
                        second_access
                            .output
                            .chain(builder)
                            .then(add_from_buffer_cb.clone())
                            .connect(scope.terminate);
                    },
                    |err| err.connect(access.input),
                );
        });

        let mut promise =
            context.command(|commands| commands.request((2.0, 3.0), workflow).take_response());

        context.run_with_conditions(&mut promise, Duration::from_secs(2));
        assert!(promise
            .take()
            .available()
            .is_some_and(|value| value.is_err_and(|n| n == 5.0)));
        assert!(context.no_unhandled_errors());
    }

    fn add_from_buffer(
        In((lhs, key)): In<(f64, BufferKey<f64>)>,
        mut access: BufferAccessMut<f64>,
    ) -> Result<f64, f64> {
        let rhs = access.get_mut(&key).map_err(|_| lhs)?.pull().ok_or(lhs)?;
        Ok(lhs + rhs)
    }

    fn multiply_buffers_by_copy(
        In((key_a, key_b)): In<(BufferKey<f64>, BufferKey<f64>)>,
        access: BufferAccess<f64>,
    ) -> f64 {
        *access.get(&key_a).unwrap().oldest().unwrap()
            * *access.get(&key_b).unwrap().oldest().unwrap()
    }

    fn add_buffers_by_pull(
        In((key_a, key_b)): In<(BufferKey<f64>, BufferKey<f64>)>,
        mut access: BufferAccessMut<f64>,
    ) -> Option<f64> {
        if access.get(&key_a).unwrap().is_empty() {
            return None;
        }

        if access.get(&key_b).unwrap().is_empty() {
            return None;
        }

        let rhs = access.get_mut(&key_a).unwrap().pull().unwrap();
        let lhs = access.get_mut(&key_b).unwrap().pull().unwrap();
        Some(rhs + lhs)
    }

    #[test]
    fn test_buffer_key_lifecycle() {
        let mut context = TestingContext::minimal_plugins();

        // Test a workflow where each node in a long chain repeatedly accesses
        // a buffer and might be the one to push a value into it.
        let workflow = context.spawn_io_workflow(|scope, builder| {
            let buffer = builder.create_buffer::<Register>(BufferSettings::keep_all());

            // The only path to termination is from listening to the buffer.
            builder
                .listen(buffer)
                .then(pull_register_from_buffer.into_blocking_callback())
                .dispose_on_none()
                .connect(scope.terminate);

            let decrement_register_cb = decrement_register.into_blocking_callback();
            let async_decrement_register_cb = async_decrement_register.as_callback();
            scope
                .input
                .chain(builder)
                .with_access(buffer)
                .then(decrement_register_cb.clone())
                .with_access(buffer)
                .then(async_decrement_register_cb.clone())
                .dispose_on_none()
                .with_access(buffer)
                .then(decrement_register_cb.clone())
                .with_access(buffer)
                .then(async_decrement_register_cb)
                .unused();
        });

        run_register_test(workflow, 0, true, &mut context);
        run_register_test(workflow, 1, true, &mut context);
        run_register_test(workflow, 2, true, &mut context);
        run_register_test(workflow, 3, true, &mut context);
        run_register_test(workflow, 4, false, &mut context);
        run_register_test(workflow, 5, false, &mut context);
        run_register_test(workflow, 6, false, &mut context);

        // Test a workflow where only one buffer accessor node is used, but the
        // key is passed through a long chain in the workflow, with a disposal
        // being forced as well.
        let workflow = context.spawn_io_workflow(|scope, builder| {
            let buffer = builder.create_buffer::<Register>(BufferSettings::keep_all());

            // The only path to termination is from listening to the buffer.
            builder
                .listen(buffer)
                .then(pull_register_from_buffer.into_blocking_callback())
                .dispose_on_none()
                .connect(scope.terminate);

            let decrement_register_and_pass_keys_cb =
                decrement_register_and_pass_keys.into_blocking_callback();
            let async_decrement_register_and_pass_keys_cb =
                async_decrement_register_and_pass_keys.as_callback();
            let (loose_end, dead_end): (_, Output<Option<Register>>) = scope
                .input
                .chain(builder)
                .with_access(buffer)
                .then(decrement_register_and_pass_keys_cb.clone())
                .then(async_decrement_register_and_pass_keys_cb.clone())
                .dispose_on_none()
                .map_block(|v| (v, None))
                .unzip();

            // Force the workflow to trigger a disposal while the key is still in flight
            dead_end.chain(builder).dispose_on_none().unused();

            loose_end
                .chain(builder)
                .then(async_decrement_register_and_pass_keys_cb)
                .dispose_on_none()
                .then(decrement_register_and_pass_keys_cb)
                .unused();
        });

        run_register_test(workflow, 0, true, &mut context);
        run_register_test(workflow, 1, true, &mut context);
        run_register_test(workflow, 2, true, &mut context);
        run_register_test(workflow, 3, true, &mut context);
        run_register_test(workflow, 4, false, &mut context);
        run_register_test(workflow, 5, false, &mut context);
        run_register_test(workflow, 6, false, &mut context);
    }

    fn run_register_test(
        workflow: Service<Register, Register>,
        initial_value: u64,
        expect_success: bool,
        context: &mut TestingContext,
    ) {
        let mut promise = context.command(|commands| {
            commands
                .request(Register::new(initial_value), workflow)
                .take_response()
        });

        context.run_while_pending(&mut promise);
        if expect_success {
            assert!(promise
                .take()
                .available()
                .is_some_and(|r| r.finished_with(initial_value)));
        } else {
            assert!(promise.take().is_cancelled());
        }
        assert!(context.no_unhandled_errors());
    }

    // We use this struct to keep track of operations that have occurred in the
    // test workflow. Values from in_slot get moved to out_slot until in_slot
    // reaches 0, then the whole struct gets put into a buffer where a listener
    // will then send it to the terminal node.
    #[derive(Clone, Copy, Debug)]
    struct Register {
        in_slot: u64,
        out_slot: u64,
    }

    impl Register {
        fn new(start_from: u64) -> Self {
            Self {
                in_slot: start_from,
                out_slot: 0,
            }
        }

        fn finished_with(&self, out_slot: u64) -> bool {
            self.in_slot == 0 && self.out_slot == out_slot
        }
    }

    fn pull_register_from_buffer(
        In(key): In<BufferKey<Register>>,
        mut access: BufferAccessMut<Register>,
    ) -> Option<Register> {
        access.get_mut(&key).ok()?.pull()
    }

    fn decrement_register(
        In((mut register, key)): In<(Register, BufferKey<Register>)>,
        mut access: BufferAccessMut<Register>,
    ) -> Register {
        if register.in_slot == 0 {
            access.get_mut(&key).unwrap().push(register);
            return register;
        }

        register.in_slot -= 1;
        register.out_slot += 1;
        register
    }

    fn decrement_register_and_pass_keys(
        In((mut register, key)): In<(Register, BufferKey<Register>)>,
        mut access: BufferAccessMut<Register>,
    ) -> (Register, BufferKey<Register>) {
        if register.in_slot == 0 {
            access.get_mut(&key).unwrap().push(register);
            return (register, key);
        }

        register.in_slot -= 1;
        register.out_slot += 1;
        (register, key)
    }

    fn async_decrement_register(
        In(input): In<AsyncCallback<(Register, BufferKey<Register>)>>,
    ) -> impl Future<Output = Option<Register>> {
        async move {
            input
                .channel
                .query(input.request, decrement_register.into_blocking_callback())
                .await
                .available()
        }
    }

    fn async_decrement_register_and_pass_keys(
        In(input): In<AsyncCallback<(Register, BufferKey<Register>)>>,
    ) -> impl Future<Output = Option<(Register, BufferKey<Register>)>> {
        async move {
            input
                .channel
                .query(
                    input.request,
                    decrement_register_and_pass_keys.into_blocking_callback(),
                )
                .await
                .available()
        }
    }

    #[test]
    fn test_buffer_key_gate_control() {
        let mut context = TestingContext::minimal_plugins();

        let workflow = context.spawn_io_workflow(|scope, builder| {
            let service = builder.commands().spawn_service(gate_access_test_open_loop);

            let buffer = builder.create_buffer(BufferSettings::keep_all());
            builder.connect(scope.input, buffer.input_slot());
            builder
                .listen(buffer)
                .then_gate_close(buffer)
                .then(service)
                .fork_unzip((
                    |chain: Chain<_>| chain.dispose_on_none().connect(buffer.input_slot()),
                    |chain: Chain<_>| chain.dispose_on_none().connect(scope.terminate),
                ));
        });

        let mut promise = context.command(|commands| commands.request(0, workflow).take_response());

        context.run_with_conditions(&mut promise, Duration::from_secs(2));
        assert!(promise.take().available().is_some_and(|v| v == 5));
        assert!(context.no_unhandled_errors());
    }

    /// Used to verify that when a key is used to open a buffer gate, it will not
    /// trigger the key's listener to wake up again.
    fn gate_access_test_open_loop(
        In(BlockingService { request: key, .. }): BlockingServiceInput<BufferKey<u64>>,
        mut access: BufferAccessMut<u64>,
        mut gate_access: BufferGateAccessMut,
    ) -> (Option<u64>, Option<u64>) {
        // We should never see a spurious wake-up in this service because the
        // gate opening is done by the key of this service.
        let mut buffer = access.get_mut(&key).unwrap();
        let value = buffer.pull().unwrap();

        // The gate should have previously been closed before reaching this
        // service
        let mut gate = gate_access.get_mut(key).unwrap();
        assert_eq!(gate.get(), Gate::Closed);
        // Open the gate, which would normally trigger a notice, but the notice
        // should not come to this service because we're using the key without
        // closed loops allowed.
        gate.open_gate();

        if value >= 5 {
            (None, Some(value))
        } else {
            (Some(value + 1), None)
        }
    }

    #[test]
    fn test_closed_loop_key_access() {
        let mut context = TestingContext::minimal_plugins();

        let delay = context.spawn_delay(Duration::from_secs_f32(0.1));

        let workflow = context.spawn_io_workflow(|scope, builder| {
            let service = builder
                .commands()
                .spawn_service(gate_access_test_closed_loop);

            let buffer = builder.create_buffer(BufferSettings::keep_all());
            builder.connect(scope.input, buffer.input_slot());
            builder.listen(buffer).then(service).fork_unzip((
                |chain: Chain<_>| {
                    chain
                        .dispose_on_none()
                        .then(delay)
                        .connect(buffer.input_slot())
                },
                |chain: Chain<_>| chain.dispose_on_none().connect(scope.terminate),
            ));
        });

        let mut promise = context.command(|commands| commands.request(3, workflow).take_response());

        context.run_with_conditions(&mut promise, Duration::from_secs(2));
        assert!(promise.take().available().is_some_and(|v| v == 0));
        assert!(context.no_unhandled_errors());
    }

    /// Used to verify that we get spurious wakeups when closed loops are allowed
    fn gate_access_test_closed_loop(
        In(BlockingService { request: key, .. }): BlockingServiceInput<BufferKey<u64>>,
        mut access: BufferAccessMut<u64>,
    ) -> (Option<u64>, Option<u64>) {
        let mut buffer = access.get_mut(&key).unwrap().allow_closed_loops();
        if let Some(value) = buffer.pull() {
            (Some(value + 1), None)
        } else {
            (None, Some(0))
        }
    }
}
