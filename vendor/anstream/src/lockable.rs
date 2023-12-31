/// Explicitly lock a [`std::io::Write`]able
pub trait Lockable {
    type Locked;

    /// Get exclusive access to the `AutoStream`
    ///
    /// Why?
    /// - Faster performance when writing in a loop
    /// - Avoid other threads interleaving output with the current thread
    fn lock(self) -> Self::Locked;
}

impl Lockable for std::io::Stdout {
    type Locked = std::io::StdoutLock<'static>;

    #[inline]
    fn lock(self) -> Self::Locked {
        #[allow(clippy::needless_borrow)] // Its needed to avoid recursion
        (&self).lock()
    }
}

impl Lockable for std::io::Stderr {
    type Locked = std::io::StderrLock<'static>;

    #[inline]
    fn lock(self) -> Self::Locked {
        #[allow(clippy::needless_borrow)] // Its needed to avoid recursion
        (&self).lock()
    }
}

#[cfg(all(windows, feature = "wincon"))]
impl Lockable for anstyle_wincon::Console<std::io::Stdout> {
    type Locked = anstyle_wincon::Console<std::io::StdoutLock<'static>>;

    #[inline]
    fn lock(self) -> Self::Locked {
        self.lock()
    }
}

#[cfg(all(windows, feature = "wincon"))]
impl Lockable for anstyle_wincon::Console<std::io::Stderr> {
    type Locked = anstyle_wincon::Console<std::io::StderrLock<'static>>;

    #[inline]
    fn lock(self) -> Self::Locked {
        self.lock()
    }
}
