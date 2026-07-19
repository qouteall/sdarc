use std::ops::Deref;
use std::ptr::{null_mut, NonNull};
use std::sync::atomic::{AtomicPtr, Ordering};
use crate::atomic_sdarc::{borrow_from_atomic_ptr_using_hazard_pointer, load_atomic_ptr_owned, HazardPointerGuard, AtomicNullableSdarc};
use crate::sdarc::{Sdarc, SdarcInner, SdarcInnerPtrErased};

pub(crate) struct WeakSdarcInner<T> {
    /// There is a circular reference. `SdarcInner` has `Sdarc<WeakSdarcInner>`, this references back.
    /// When initialized, it's not null.
    /// When collector thinks that a `SdarcInner`'s strong count sum reach zero (observed zero sum once, clear tags, observed zero sum in next iteration with no tag set),
    /// If [`SdarcInner::weak_inner_ref`] is initialized, this backref will be set to null.
    /// Upgrade can only succeed if it's not null, and upgrade is under reader critical section.
    ///
    /// Note: it's possible that a concurrent upgrade resurrects the SdarcInner whose strong count sum is 0.
    /// After resurrection, `Sdarc` can still downgrade.
    /// The `WeakSdarc` may be unable to upgrade or may can upgrade after resurrection,
    /// depending on whether backref is cleared, which depends on collector timing.
    ///
    /// It's similar to [`AtomicNullableSdarc`], except that it doesn't own a reference count.
    back_ref: AtomicPtr<SdarcInner<T>>,
}

unsafe impl<T: Send> Send for WeakSdarcInner<T> {}
unsafe impl<T: Sync> Sync for WeakSdarcInner<T> {}

impl<T> Drop for WeakSdarcInner<T> {
    fn drop(&mut self) {
        // use Relaxed ordering because it's just an assertion
        assert!(
            self.back_ref.load(Ordering::Relaxed).is_null(),
            "WeakSdarcInner's backref is not cleared"
        );
    }
}

/// The weak reference version of [`Sdarc`].
///
/// The weak reference behavior is very different to std `Arc` and `Weak`.
/// When there is no strong reference of `Sdarc`, the [`WeakSdarc::upgrade`] may still succeed.
/// Then the dead `Sdarc` will be resurrected.
///
/// Why have the weird resurrection mechanism, instead of ensuring that resurrection is not possible:
/// Avoiding resurrection requires [`WeakSdarc::upgrade`] to ensure whether strong count sum is 0 immediately.
/// Without locking, it's not possible. We avoid locking of counters to improve scalability.
pub struct WeakSdarc<T> {
    sdarc_weak_inner: Sdarc<WeakSdarcInner<T>>,
}

pub(crate) enum ClearWeakBackRefResult {
    WeakRefNotInvolved,
    WeakBackRefCleared,
    WeakBackRefWasAlreadyNull,
}

/// When this function is called, the strong count sum reaches 0.
/// But there may be weak references, and the weak references can still upgrade at the same time.
///
/// But the [`SdarcInner::weak_inner_ref`] will never be initialized at that time if it is not initialized,
/// because it can only be initialized from strong reference, and strong reference doesn't exist
/// if no weak reference to it exists.
///
/// If [`SdarcInner::weak_inner_ref`] has been initialized, it will clear the backref.
/// After clearing, weak ref's upgrade will fail. And the backref will never become non-null again.
///
/// If the `Sdarc` has never been downgraded, it will return [`ClearWeakBackRefResult::WeakRefNotInvolved`],
/// and the collector will free it once strong count sum reaches 0 and counters keeps being same across one iteration.
///
/// If the `Sdarc` has been downgraded, and it's the first time that `clear_weak_backref_impl` get called for it,
/// then it will return [`ClearWeakBackRefResult::WeakBackRefCleared`],
/// and the collector will assume that it may resurrect, and will not free despite strong counter sum being 0 and not changing.
///
/// If the `Sdarc` has been downgraded, then resurrected, then `clear_weak_backref_impl` may be called for it again.
/// In that case, the backref has already been cleared. No more upgrade is possible. The collector will free it
/// once strong count sum reaches 0 and counters keep being same across one iteration.
///
/// Note that if it dies then resurrects quickly, without the "confirmed dead" state being observed by collector,
/// then this function won't be called at that time.
pub(crate) fn clear_weak_backref_impl<T>(ptr: SdarcInnerPtrErased) -> ClearWeakBackRefResult {
    let p: NonNull<SdarcInner<T>> = ptr.into_typed::<T>();

    let r: &SdarcInner<T> = unsafe { p.as_ref() };

    if let Some(inner) = r.weak_inner_ref.get() {
        /// Reset the backref to null. the weak ref will no longer be able to upgrade.
        /// The clearing is one-way. after clearing, it cannot become non-null.
        ///
        /// Why use Relaxed ordering: see comment in [`WeakSdarc::upgrade`]
        let swapped_ptr = inner.back_ref.swap(null_mut(), Ordering::Relaxed);
        if swapped_ptr.is_null() {
            ClearWeakBackRefResult::WeakBackRefWasAlreadyNull
        } else {
            ClearWeakBackRefResult::WeakBackRefCleared
        }
    } else {
        /// When this function is called, the strong count sum reaches 0.
        /// It's only initialized in [`Sdarc::downgrade`] which requires a strong reference.
        /// So if it's not initialized now, it will never initialize, then there will be no weak ref to it,
        /// and no upgrade is possible.
        ClearWeakBackRefResult::WeakRefNotInvolved
    }
}

impl<T: Send + Sync> Sdarc<T> {
    pub fn downgrade(&self) -> WeakSdarc<T> {
        let inner_ptr = self.inner_ptr;
        let inner = self.inner_ref();
        let r: &Sdarc<WeakSdarcInner<T>> = inner.weak_inner_ref.get_or_init(|| {
            Sdarc::new(WeakSdarcInner {
                back_ref: AtomicPtr::new(inner_ptr.as_ptr()),
            })
        });
        WeakSdarc {
            sdarc_weak_inner: r.clone(),
        }
    }
}

impl<T> Clone for WeakSdarc<T> {
    fn clone(&self) -> Self {
        Self {
            sdarc_weak_inner: self.sdarc_weak_inner.clone(),
        }
    }
}

impl<T: Send + Sync> WeakSdarc<T> {
    /// Unlike std `Arc` and `Weak`, `Sdarc` and `WeakSdarc` have resurrection mechanism.
    /// Even after strong count sum reaches zero, upgrade may still succeed, then it will be resurrected.
    ///
    /// If the strong count sum has reached 0, then it's not deterministic whether upgrade will succeed
    /// (depending on collector timing). Upgrade may fail despite there exists strong references to same pointee.
    ///
    /// If the strong count sum never reaches 0, upgrade will succeed.
    pub fn upgrade(&self) -> Option<Sdarc<T>> {
        let weak_inner: &WeakSdarcInner<T> = self.sdarc_weak_inner.deref();
        load_atomic_ptr_owned(&weak_inner.back_ref)
    }

    /// Unlike std `Weak`, the `WeakSdarc` can be borrowed without upgrading.
    /// The pointee is kept alive using hazard pointer and "debt paying" mechanism.
    #[allow(clippy::needless_lifetimes)]
    pub fn borrow<'a>(&'a self) -> Option<HazardPointerGuard<'a, T>> {
        let weak_inner: &WeakSdarcInner<T> = self.sdarc_weak_inner.deref();
        borrow_from_atomic_ptr_using_hazard_pointer(&weak_inner.back_ref)
    }
}
