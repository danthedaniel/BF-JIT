use std::cell::RefCell;
use std::io::{self, Write};
use std::rc::Rc;

/// Writeable buffer that tracks what was written to it. Used for testing.
pub struct SharedBuffer {
    inner: Rc<RefCell<Vec<u8>>>,
}

impl SharedBuffer {
    pub fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(Vec::new())),
        }
    }

    pub fn clone(&self) -> Self {
        Self {
            inner: Rc::clone(&self.inner),
        }
    }

    pub fn get_string_content(&self) -> String {
        let data = self.inner.borrow().clone();
        String::from_utf8(data).expect("Data was invalid utf-8")
    }
}

impl Write for SharedBuffer {
    fn write(&mut self, buf: &[u8]) -> Result<usize, io::Error> {
        self.inner.borrow_mut().write(buf)
    }

    fn flush(&mut self) -> Result<(), io::Error> {
        self.inner.borrow_mut().flush()
    }
}
