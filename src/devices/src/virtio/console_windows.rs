use std::borrow::Cow;
use std::io;
use std::sync::{Arc, Mutex};

use super::{ActivateError, ActivateResult, DeviceState, InterruptTransport, Queue, VirtioDevice};
use polly::event_manager::{EventManager, Subscriber};
use utils::epoll::{EpollEvent, EventSet};
use utils::eventfd::{EventFd, EFD_NONBLOCK};
use vm_memory::{GuestMemory, GuestMemoryMmap};

pub const TYPE_CONSOLE: u32 = 3;

pub mod port_io {
    use std::io::{self, ErrorKind};
    use std::sync::{Arc, Mutex};
    use vm_memory::{bitmap::Bitmap, VolatileSlice};
    use windows::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
    use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
    use windows::Win32::System::Console::{
        GetConsoleMode, GetConsoleScreenBufferInfo, GetStdHandle, SetConsoleMode,
        CONSOLE_MODE, CONSOLE_SCREEN_BUFFER_INFO, ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT,
        ENABLE_PROCESSED_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_ERROR_HANDLE,
        STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };

    pub trait PortInput: Send {
        fn read_volatile(&mut self, buf: &mut VolatileSlice) -> io::Result<usize>;
        fn wait_until_readable(&self, _stopfd: Option<&utils::eventfd::EventFd>);
    }

    pub trait PortOutput: Send {
        fn write_volatile(&mut self, buf: &VolatileSlice) -> io::Result<usize>;
        fn wait_until_writable(&self);
    }

    pub trait PortTerminalProperties: Send + Sync {
        fn get_win_size(&self) -> (u16, u16);
    }

    struct EmptyInput;
    impl PortInput for EmptyInput {
        fn read_volatile(&mut self, _buf: &mut VolatileSlice) -> io::Result<usize> {
            Ok(0)
        }
        fn wait_until_readable(&self, _stopfd: Option<&utils::eventfd::EventFd>) {}
    }

    struct FixedTerm(u16, u16);
    impl PortTerminalProperties for FixedTerm {
        fn get_win_size(&self) -> (u16, u16) {
            (self.0, self.1)
        }
    }

    struct ConsoleInput {
        handle: HANDLE,
        original_mode: CONSOLE_MODE,
    }

    impl ConsoleInput {
        fn new(handle: HANDLE) -> io::Result<Self> {
            if handle == INVALID_HANDLE_VALUE {
                return Err(io::Error::new(ErrorKind::NotFound, "Invalid console handle"));
            }

            let mut mode = CONSOLE_MODE(0);
            unsafe {
                GetConsoleMode(handle, &mut mode)
                    .map_err(|e| io::Error::other(format!("GetConsoleMode failed: {e}")))?;
            }

            // Disable line input, echo, and processed input for raw mode
            let raw_mode = CONSOLE_MODE(
                mode.0 & !(ENABLE_LINE_INPUT.0 | ENABLE_ECHO_INPUT.0 | ENABLE_PROCESSED_INPUT.0),
            );

            unsafe {
                SetConsoleMode(handle, raw_mode)
                    .map_err(|e| io::Error::other(format!("SetConsoleMode failed: {e}")))?;
            }

            Ok(Self {
                handle,
                original_mode: mode,
            })
        }
    }

    impl Drop for ConsoleInput {
        fn drop(&mut self) {
            unsafe {
                let _ = SetConsoleMode(self.handle, self.original_mode);
            }
        }
    }

    // SAFETY: HANDLE is a Win32 handle. Console handles are process-global and
    // safe to use from multiple threads when protected by external synchronization.
    unsafe impl Send for ConsoleInput {}

    impl PortInput for ConsoleInput {
        fn read_volatile(&mut self, buf: &mut VolatileSlice) -> io::Result<usize> {
            let guard = buf.ptr_guard_mut();
            let dst = guard.as_ptr();
            let mut bytes_read = 0u32;

            unsafe {
                ReadFile(
                    self.handle,
                    Some(std::slice::from_raw_parts_mut(dst, buf.len())),
                    Some(&mut bytes_read),
                    None,
                )
                .map_err(|e| io::Error::other(format!("ReadFile failed: {e}")))?;
            }

            let bytes_read = bytes_read as usize;
            buf.bitmap().mark_dirty(0, bytes_read);
            Ok(bytes_read)
        }

        fn wait_until_readable(&self, stopfd: Option<&utils::eventfd::EventFd>) {
            use windows::Win32::Foundation::HANDLE;
            use windows::Win32::System::Threading::{WaitForMultipleObjects, INFINITE};

            let mut handles = vec![self.handle];
            if let Some(fd) = stopfd {
                handles.push(HANDLE(fd.as_raw_handle()));
            }
            // Wait until stdin or the stop signal is readable.
            // The return value indicates which object was signalled; the caller
            // is responsible for checking whether the stop flag is set.
            unsafe {
                let _ = WaitForMultipleObjects(&handles, false, INFINITE);
            }
        }
    }

    struct ConsoleOutput {
        handle: HANDLE,
    }

    // SAFETY: Console output handles are process-global and safe to send across threads.
    unsafe impl Send for ConsoleOutput {}

    impl PortOutput for ConsoleOutput {
        fn write_volatile(&mut self, buf: &VolatileSlice) -> io::Result<usize> {
            let guard = buf.ptr_guard();
            let src = guard.as_ptr();
            let mut bytes_written = 0u32;

            unsafe {
                WriteFile(
                    self.handle,
                    Some(std::slice::from_raw_parts(src, buf.len())),
                    Some(&mut bytes_written),
                    None,
                )
                .map_err(|e| io::Error::other(format!("WriteFile failed: {e}")))?;
            }

            Ok(bytes_written as usize)
        }

        fn wait_until_writable(&self) {
            // Windows console is always writable
        }
    }

    struct ConsoleTerm {
        handle: HANDLE,
    }

    // SAFETY: Console terminal handles are process-global and safe to share/send across threads.
    unsafe impl Send for ConsoleTerm {}
    unsafe impl Sync for ConsoleTerm {}

    impl PortTerminalProperties for ConsoleTerm {
        fn get_win_size(&self) -> (u16, u16) {
            let mut info = CONSOLE_SCREEN_BUFFER_INFO::default();
            unsafe {
                if GetConsoleScreenBufferInfo(self.handle, &mut info).is_ok() {
                    let width = (info.srWindow.Right - info.srWindow.Left + 1) as u16;
                    let height = (info.srWindow.Bottom - info.srWindow.Top + 1) as u16;
                    return (width, height);
                }
            }
            (80, 24) // Default fallback
        }
    }

    pub fn input_empty() -> io::Result<Box<dyn PortInput + Send>> {
        Ok(Box::new(EmptyInput))
    }

    pub fn input_to_raw_fd_dup(fd: i32) -> io::Result<Box<dyn PortInput + Send>> {
        let handle = if fd == 0 {
            unsafe { GetStdHandle(STD_INPUT_HANDLE) }
                .map_err(|e| io::Error::other(format!("GetStdHandle failed: {e}")))?
        } else {
            // Convert CRT fd → owned HANDLE via DuplicateHandle.
            extern "C" {
                fn _get_osfhandle(fd: i32) -> isize;
            }
            let raw = unsafe { _get_osfhandle(fd) };
            if raw == -1isize {
                return Err(io::Error::new(ErrorKind::InvalidInput, "invalid fd"));
            }
            let src = HANDLE(raw as *mut _);
            let mut dup = HANDLE::default();
            let proc = unsafe { windows::Win32::System::Threading::GetCurrentProcess() };
            unsafe {
                windows::Win32::Foundation::DuplicateHandle(
                    proc,
                    src,
                    proc,
                    &mut dup,
                    0,
                    false,
                    windows::Win32::Foundation::DUPLICATE_SAME_ACCESS,
                )
            }
            .map_err(|e| {
                io::Error::other(format!("DuplicateHandle failed: {e}"))
            })?;
            dup
        };

        // Console handles: use ConsoleInput (raw mode + proper wait).
        if let Ok(ci) = ConsoleInput::new(handle) {
            return Ok(Box::new(ci));
        }

        // Non-console (pipe / file): use File-based input.
        // For fd=0 stdin pipe, the GetStdHandle-returned handle is NOT owned — avoid
        // wrapping it in File (which would close it).  Return EmptyInput instead,
        // as piped stdin in a VM-host context is rarely meaningful for guest I/O.
        if fd == 0 {
            return Ok(Box::new(EmptyInput));
        }

        // We own the duplicated handle — wrap as File for ReadFile + WaitForMultipleObjects.
        use std::os::windows::io::FromRawHandle;
        let file = unsafe { std::fs::File::from_raw_handle(handle.0 as *mut _) };
        Ok(Box::new(FileOrPipeInput { file }))
    }

    /// Readable wrapper around an owned file/pipe handle.
    struct FileOrPipeInput {
        file: std::fs::File,
    }

    // SAFETY: std::fs::File is Send.
    unsafe impl Send for FileOrPipeInput {}

    impl PortInput for FileOrPipeInput {
        fn read_volatile(&mut self, buf: &mut VolatileSlice) -> io::Result<usize> {
            use std::io::Read;
            let guard = buf.ptr_guard_mut();
            let data = unsafe { std::slice::from_raw_parts_mut(guard.as_ptr(), buf.len()) };
            let n = self.file.read(data)?;
            buf.bitmap().mark_dirty(0, n);
            Ok(n)
        }

        fn wait_until_readable(&self, stopfd: Option<&utils::eventfd::EventFd>) {
            use std::os::windows::io::AsRawHandle;
            use windows::Win32::System::Threading::{WaitForMultipleObjects, INFINITE};
            let handle = HANDLE(self.file.as_raw_handle() as *mut _);
            let mut handles = vec![handle];
            if let Some(fd) = stopfd {
                handles.push(HANDLE(fd.as_raw_handle()));
            }
            unsafe {
                let _ = WaitForMultipleObjects(&handles, false, INFINITE);
            }
        }
    }

    pub fn output_to_raw_fd_dup(fd: i32) -> io::Result<Box<dyn PortOutput + Send>> {
        let std_handle_type = if fd == 1 {
            Some(STD_OUTPUT_HANDLE)
        } else if fd == 2 {
            Some(STD_ERROR_HANDLE)
        } else {
            None
        };

        let handle = if let Some(sht) = std_handle_type {
            unsafe { GetStdHandle(sht) }
                .map_err(|e| io::Error::other(format!("GetStdHandle failed: {e}")))?
        } else {
            // Convert CRT fd to HANDLE and duplicate it so we own it.
            extern "C" {
                fn _get_osfhandle(fd: i32) -> isize;
            }
            let raw = unsafe { _get_osfhandle(fd) };
            if raw == -1isize {
                return Err(io::Error::new(ErrorKind::InvalidInput, "invalid fd"));
            }
            let src_handle = HANDLE(raw as *mut _);
            let mut dup = HANDLE::default();
            let proc = unsafe { windows::Win32::System::Threading::GetCurrentProcess() };
            unsafe {
                windows::Win32::Foundation::DuplicateHandle(
                    proc,
                    src_handle,
                    proc,
                    &mut dup,
                    0,
                    false,
                    windows::Win32::Foundation::DUPLICATE_SAME_ACCESS,
                )
            }
            .map_err(|e| {
                io::Error::other(format!("DuplicateHandle failed: {e}"))
            })?;
            dup
        };

        // Try console path first (enables VT100 as a side-effect).
        let mut mode = CONSOLE_MODE(0);
        if unsafe { GetConsoleMode(handle, &mut mode).is_ok() } {
            let vt_mode = CONSOLE_MODE(mode.0 | ENABLE_VIRTUAL_TERMINAL_PROCESSING.0);
            unsafe { let _ = SetConsoleMode(handle, vt_mode); }
            return Ok(Box::new(ConsoleOutput { handle }));
        }

        // Non-console handle (pipe / file).
        if std_handle_type.is_some() {
            // We do NOT own handles returned by GetStdHandle — use Rust's std writers
            // which route through the correct Win32 handle and handle buffering correctly.
            if fd == 2 {
                return Ok(Box::new(StdErrOutput));
            }
            return Ok(Box::new(StdOutOutput));
        }

        // We own the duplicated handle — wrap as a File for proper cleanup.
        use std::os::windows::io::FromRawHandle;
        let file = unsafe { std::fs::File::from_raw_handle(handle.0 as *mut _) };
        Ok(Box::new(FileOutput(file)))
    }

    struct StdOutOutput;
    impl PortOutput for StdOutOutput {
        fn write_volatile(&mut self, buf: &VolatileSlice) -> io::Result<usize> {
            use std::io::Write;
            let guard = buf.ptr_guard();
            let data = unsafe { std::slice::from_raw_parts(guard.as_ptr(), buf.len()) };
            io::stdout().write(data)
        }
        fn wait_until_writable(&self) {}
    }

    struct StdErrOutput;
    impl PortOutput for StdErrOutput {
        fn write_volatile(&mut self, buf: &VolatileSlice) -> io::Result<usize> {
            use std::io::Write;
            let guard = buf.ptr_guard();
            let data = unsafe { std::slice::from_raw_parts(guard.as_ptr(), buf.len()) };
            io::stderr().write(data)
        }
        fn wait_until_writable(&self) {}
    }

    pub fn output_file(file: std::fs::File) -> io::Result<Box<dyn PortOutput + Send>> {
        Ok(Box::new(FileOutput(file)))
    }

    struct FileOutput(std::fs::File);

    impl PortOutput for FileOutput {
        fn write_volatile(&mut self, buf: &VolatileSlice) -> io::Result<usize> {
            use std::io::Write;
            let guard = buf.ptr_guard();
            let data = unsafe { std::slice::from_raw_parts(guard.as_ptr(), buf.len()) };
            self.0.write(data)
        }

        fn wait_until_writable(&self) {}
    }

    // SAFETY: std::fs::File is Send.
    unsafe impl Send for FileOutput {}

    pub fn output_to_log_as_err() -> Box<dyn PortOutput + Send> {
        Box::new(LogOutput::new())
    }

    pub fn term_fd(_fd: i32) -> io::Result<Box<dyn PortTerminalProperties>> {
        let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }
            .map_err(|e| io::Error::other(format!("GetStdHandle failed: {e}")))?;
        Ok(Box::new(ConsoleTerm { handle }))
    }

    pub fn term_fixed_size(cols: u16, rows: u16) -> Box<dyn PortTerminalProperties> {
        Box::new(FixedTerm(cols, rows))
    }

    struct LogOutput {
        buf: Arc<Mutex<Vec<u8>>>,
    }

    impl LogOutput {
        fn new() -> Self {
            Self {
                buf: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl PortOutput for LogOutput {
        fn write_volatile(&mut self, buf: &VolatileSlice) -> io::Result<usize> {
            let guard = buf.ptr_guard();
            let data = unsafe { std::slice::from_raw_parts(guard.as_ptr(), buf.len()) };

            let mut log_buf = self.buf.lock().unwrap();
            log_buf.extend_from_slice(data);

            let mut start = 0;
            for (i, &ch) in log_buf.iter().enumerate() {
                if ch == b'\n' {
                    let line = String::from_utf8_lossy(&log_buf[start..i]);
                    error!("init_or_kernel: {}", line);
                    start = i + 1;
                }
            }
            log_buf.drain(0..start);

            if log_buf.len() > 512 {
                let line = String::from_utf8_lossy(&log_buf);
                error!("init_or_kernel: [missing newline]{}", line);
                log_buf.clear();
            }

            Ok(buf.len())
        }

        fn wait_until_writable(&self) {}
    }
}

pub struct PortDescription {
    pub name: Cow<'static, str>,
    pub input: Option<Arc<Mutex<Box<dyn port_io::PortInput + Send>>>>,
    pub output: Option<Arc<Mutex<Box<dyn port_io::PortOutput + Send>>>>,
    pub terminal: Option<Box<dyn port_io::PortTerminalProperties>>,
}

impl PortDescription {
    pub fn console(
        input: Option<Box<dyn port_io::PortInput + Send>>,
        output: Option<Box<dyn port_io::PortOutput + Send>>,
        terminal: Box<dyn port_io::PortTerminalProperties>,
    ) -> Self {
        Self {
            name: "".into(),
            input: input.map(|i| Arc::new(Mutex::new(i))),
            output: output.map(|o| Arc::new(Mutex::new(o))),
            terminal: Some(terminal),
        }
    }

    pub fn output_pipe(
        name: impl Into<Cow<'static, str>>,
        output: Box<dyn port_io::PortOutput + Send>,
    ) -> Self {
        Self {
            name: name.into(),
            input: None,
            output: Some(Arc::new(Mutex::new(output))),
            terminal: None,
        }
    }

    pub fn input_pipe(
        name: impl Into<Cow<'static, str>>,
        input: Box<dyn port_io::PortInput + Send>,
    ) -> Self {
        Self {
            name: name.into(),
            input: Some(Arc::new(Mutex::new(input))),
            output: None,
            terminal: None,
        }
    }
}

pub struct Console {
    queues: Vec<Queue>,
    queue_events: Vec<EventFd>,
    activate_evt: EventFd,
    state: DeviceState,
    acked_features: u64,
    ports: Vec<PortDescription>,
}

impl Console {
    fn num_queues(ports: usize) -> usize {
        // Two per-port queues (rx/tx) plus control rx/tx queues.
        ports.saturating_mul(2) + 2
    }

    pub fn new(ports: Vec<PortDescription>) -> io::Result<Console> {
        let ports_len = ports.len().max(1);
        let queues = vec![Queue::new(32); Self::num_queues(ports_len)];
        let mut queue_events = Vec::with_capacity(queues.len());
        for _ in 0..queues.len() {
            queue_events.push(EventFd::new(EFD_NONBLOCK)?);
        }

        Ok(Self {
            queues,
            queue_events,
            activate_evt: EventFd::new(EFD_NONBLOCK)?,
            state: DeviceState::Inactive,
            acked_features: 0,
            ports,
        })
    }

    fn process_tx_queue(&mut self, queue_index: usize) -> bool {
        let DeviceState::Activated(ref mem, _) = self.state else {
            return false;
        };

        // TX queue: guest writes data to host
        // Queue index 3 = port 0 TX, 5 = port 1 TX, etc.
        let port_index = if queue_index >= 3 { (queue_index - 3) / 2 } else { return false };

        let output = match self.ports.get(port_index).and_then(|p| p.output.as_ref()) {
            Some(out) => out.clone(),
            None => return false,
        };

        let mut used_any = false;
        while let Some(head) = self.queues[queue_index].pop(mem) {
            let index = head.index;
            let mut used_len: u32 = 0;

            for desc in head.into_iter() {
                if desc.is_write_only() {
                    continue;
                }

                if let Ok(slice) = mem.get_slice(desc.addr, desc.len as usize) {
                    if let Ok(mut output_guard) = output.lock() {
                        match output_guard.write_volatile(&slice) {
                            Ok(written) => used_len = used_len.saturating_add(written as u32),
                            Err(e) => error!("console(windows): TX write failed: {e:?}"),
                        }
                    }
                }
            }

            if let Err(e) = self.queues[queue_index].add_used(mem, index, used_len) {
                error!("console(windows): failed to add used entry: {e:?}");
            } else {
                used_any = true;
            }
        }

        used_any
    }

    fn process_rx_queue(&mut self, queue_index: usize) -> bool {
        let DeviceState::Activated(ref mem, _) = self.state else {
            return false;
        };

        // RX queue: host writes data to guest
        // Queue index 2 = port 0 RX, 4 = port 1 RX, etc.
        let port_index = if queue_index >= 2 { (queue_index - 2) / 2 } else { return false };

        let input = match self.ports.get(port_index).and_then(|p| p.input.as_ref()) {
            Some(inp) => inp.clone(),
            None => return false,
        };

        let mut used_any = false;
        while let Some(head) = self.queues[queue_index].pop(mem) {
            let index = head.index;
            let mut total_written = 0u32;

            for desc in head.into_iter() {
                if !desc.is_write_only() {
                    continue;
                }

                if let Ok(mut slice) = mem.get_slice(desc.addr, desc.len as usize) {
                    if let Ok(mut input_guard) = input.lock() {
                        match input_guard.read_volatile(&mut slice) {
                            Ok(read) => total_written = total_written.saturating_add(read as u32),
                            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                            Err(e) => {
                                error!("console(windows): RX read failed: {e:?}");
                                break;
                            }
                        }
                    }
                }
            }

            if let Err(e) = self.queues[queue_index].add_used(mem, index, total_written) {
                error!("console(windows): failed to ack rx queue entry: {e:?}");
            } else if total_written > 0 {
                used_any = true;
            }
        }

        used_any
    }

    fn register_runtime_events(&self, event_manager: &mut EventManager) {
        let Ok(self_subscriber) = event_manager.subscriber(self.activate_evt.as_raw_fd()) else {
            return;
        };

        for evt in &self.queue_events {
            let fd = evt.as_raw_fd();
            let event = EpollEvent::new(EventSet::IN, fd as u64);
            if let Err(e) = event_manager.register(fd, event, self_subscriber.clone()) {
                error!("console(windows): failed to register queue event {fd}: {e:?}");
            }
        }

        let _ = event_manager.unregister(self.activate_evt.as_raw_fd());
    }
}

impl VirtioDevice for Console {
    fn avail_features(&self) -> u64 {
        (1 << 32) | (1 << 1)
    }

    fn acked_features(&self) -> u64 {
        self.acked_features
    }

    fn set_acked_features(&mut self, acked_features: u64) {
        self.acked_features = acked_features;
    }

    fn device_type(&self) -> u32 {
        TYPE_CONSOLE
    }

    fn device_name(&self) -> &str {
        "virtio_console_windows"
    }

    fn queues(&self) -> &[Queue] {
        &self.queues
    }

    fn queues_mut(&mut self) -> &mut [Queue] {
        &mut self.queues
    }

    fn queue_events(&self) -> &[EventFd] {
        &self.queue_events
    }

    fn read_config(&self, _offset: u64, data: &mut [u8]) {
        data.fill(0);
    }

    fn write_config(&mut self, _offset: u64, _data: &[u8]) {}

    fn activate(&mut self, mem: GuestMemoryMmap, interrupt: InterruptTransport) -> ActivateResult {
        self.state = DeviceState::Activated(mem, interrupt);
        self.activate_evt
            .write(1)
            .map_err(|_| ActivateError::BadActivate)?;
        Ok(())
    }

    fn is_activated(&self) -> bool {
        self.state.is_activated()
    }
}

impl Subscriber for Console {
    fn process(&mut self, event: &EpollEvent, event_manager: &mut EventManager) {
        let source = event.fd();
        if source == self.activate_evt.as_raw_fd() {
            let _ = self.activate_evt.read();
            self.register_runtime_events(event_manager);
            return;
        }

        if !self.is_activated() {
            return;
        }

        let mut raise_irq = false;
        for queue_index in 0..self.queue_events.len() {
            if self.queue_events[queue_index].as_raw_fd() != source {
                continue;
            }

            let _ = self.queue_events[queue_index].read();
            let is_tx_queue = queue_index >= 2 && (queue_index % 2 == 1);
            if queue_index == 3 {
                // control tx queue
                raise_irq |= self.process_tx_queue(queue_index);
            } else if queue_index == 2 {
                // control rx queue
                raise_irq |= self.process_rx_queue(queue_index);
            } else if is_tx_queue {
                raise_irq |= self.process_tx_queue(queue_index);
            } else {
                raise_irq |= self.process_rx_queue(queue_index);
            }
        }

        if raise_irq {
            self.state.signal_used_queue();
        }
    }

    fn interest_list(&self) -> Vec<EpollEvent> {
        vec![EpollEvent::new(
            EventSet::IN,
            self.activate_evt.as_raw_fd() as u64,
        )]
    }
}
