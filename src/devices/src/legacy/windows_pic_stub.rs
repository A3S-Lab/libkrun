use std::sync::{Arc, Mutex, OnceLock};

use crate::bus::BusDevice;

fn windows_pic_debug_log(message: impl AsRef<str>) {
    static VALUE: OnceLock<bool> = OnceLock::new();
    if !*VALUE.get_or_init(|| {
        std::env::var("LIBKRUN_WINDOWS_VERBOSE_DEBUG")
            .or_else(|_| std::env::var("LIBKRUN_WINDOWS_IO_DEBUG"))
            .map(|v| {
                matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false)
    }) {
        return;
    }
    use std::io::Write;

    for path in [
        r"C:\Users\18770\.a3s\libkrun-whpx-io-current.log",
        "tmp_whpx_io.log",
    ] {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(file, "{}", message.as_ref());
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InitState {
    Ready,
    ExpectIcw2,
    ExpectIcw3,
    ExpectIcw4,
}

#[derive(Clone, Copy, Debug)]
struct PicState {
    vector_offset: u8,
    mask: u8,
    init_state: InitState,
    initialized: bool,
    auto_eoi: bool,
    irr: u8,
    in_service: u8,
}

impl Default for PicState {
    fn default() -> Self {
        Self {
            vector_offset: 0,
            mask: 0xff,
            init_state: InitState::Ready,
            initialized: false,
            auto_eoi: false,
            irr: 0,
            in_service: 0,
        }
    }
}

impl PicState {
    fn clear_pending_irq_without_in_service(&mut self, irq: u8) {
        if irq < 8 && (self.in_service & (1 << irq)) == 0 {
            self.irr &= !(1 << irq);
        }
    }

    fn begin_init(&mut self, icw1: u8, name: &str) {
        self.init_state = InitState::ExpectIcw2;
        self.initialized = false;
        self.auto_eoi = false;
        self.irr = 0;
        self.in_service = 0;
        log::debug!(
            "PIC {}: ICW1=0x{:02x} init started (expect_icw4={})",
            name,
            icw1,
            (icw1 & 0x01) != 0
        );
        windows_pic_debug_log(format!(
            "[PIC] {} icw1=0x{:02x} init expect_icw4={}",
            name,
            icw1,
            (icw1 & 0x01) != 0
        ));
    }

    fn handle_command_write(&mut self, value: u8, name: &str) {
        if (value & 0x10) != 0 {
            self.begin_init(value, name);
            return;
        }

        match value & 0xe0 {
            0x20 => {
                let irq = self.in_service.trailing_zeros() as u8;
                if irq < 8 {
                    self.in_service &= !(1 << irq);
                } else {
                    let pending_irq = self.irr.trailing_zeros() as u8;
                    self.clear_pending_irq_without_in_service(pending_irq);
                }
                log::debug!("PIC {}: nonspecific EOI command 0x{:02x}", name, value);
                windows_pic_debug_log(format!(
                    "[PIC] {} eoi cmd=0x{:02x} specific=false irr=0x{:02x} in_service=0x{:02x}",
                    name, value, self.irr, self.in_service
                ));
            }
            0x60 => {
                let irq = value & 0x07;
                if (self.in_service & (1 << irq)) != 0 {
                    self.in_service &= !(1 << irq);
                } else {
                    // Windows fixed-IRQ fallback can skip the pre-delivery PIC
                    // acknowledge and rely on the guest's EOI sequence instead.
                    // In that mode, clear the pending IRR bit here so the next PIT
                    // edge becomes deliverable again.
                    self.clear_pending_irq_without_in_service(irq);
                }
                log::debug!(
                    "PIC {}: specific EOI command 0x{:02x} irq={}",
                    name,
                    value,
                    irq
                );
                windows_pic_debug_log(format!(
                    "[PIC] {} eoi cmd=0x{:02x} specific=true irq={} irr=0x{:02x} in_service=0x{:02x}",
                    name, value, irq, self.irr, self.in_service
                ));
            }
            _ => {}
        }
    }

    fn handle_data_write(&mut self, value: u8, name: &str) {
        match self.init_state {
            InitState::ExpectIcw2 => {
                self.vector_offset = value;
                self.init_state = InitState::ExpectIcw3;
                log::debug!(
                    "PIC {}: ICW2 vector base set to 0x{:02x}",
                    name,
                    self.vector_offset
                );
                windows_pic_debug_log(format!(
                    "[PIC] {} icw2 vector_base=0x{:02x}",
                    name, self.vector_offset
                ));
            }
            InitState::ExpectIcw3 => {
                self.init_state = InitState::ExpectIcw4;
                log::debug!("PIC {}: ICW3=0x{:02x}", name, value);
                windows_pic_debug_log(format!("[PIC] {} icw3=0x{:02x}", name, value));
            }
            InitState::ExpectIcw4 => {
                self.auto_eoi = (value & 0x02) != 0;
                self.initialized = true;
                self.init_state = InitState::Ready;
                log::debug!(
                    "PIC {}: ICW4=0x{:02x} initialized auto_eoi={}",
                    name,
                    value,
                    self.auto_eoi
                );
                windows_pic_debug_log(format!(
                    "[PIC] {} icw4=0x{:02x} initialized auto_eoi={}",
                    name, value, self.auto_eoi
                ));
            }
            InitState::Ready => {
                self.mask = value;
                log::debug!(
                    "PIC {}: OCW1 mask set to 0x{:02x} (irq0_masked={})",
                    name,
                    self.mask,
                    (self.mask & 0x01) != 0
                );
                windows_pic_debug_log(format!("[PIC] {} ocw1 mask=0x{:02x}", name, self.mask));
            }
        }
    }

    fn read_command(&self) -> u8 {
        self.in_service
    }

    fn read_data(&self) -> u8 {
        self.mask
    }

    fn vector_for_irq(&self, irq: u8) -> Option<u8> {
        if !self.initialized
            || irq >= 8
            || (self.mask & (1 << irq)) != 0
            || (self.irr & (1 << irq)) == 0
            || (self.in_service & (1 << irq)) != 0
        {
            return None;
        }
        Some(self.vector_offset.wrapping_add(irq))
    }

    fn raise_irq(&mut self, irq: u8) {
        if irq < 8 {
            self.irr |= 1 << irq;
        }
    }

    fn mark_in_service(&mut self, irq: u8) {
        if irq < 8 {
            self.irr &= !(1 << irq);
            if !self.auto_eoi {
                self.in_service |= 1 << irq;
            }
        }
    }
}

#[derive(Default)]
struct PicPair {
    master: PicState,
    slave: PicState,
}

fn global_pic_state() -> &'static Mutex<PicPair> {
    static STATE: OnceLock<Mutex<PicPair>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(PicPair::default()))
}

pub fn query_irq_vector(irq: u8) -> Option<u8> {
    let pics = global_pic_state().lock().ok()?;
    if irq < 8 {
        let vector = pics.master.vector_for_irq(irq);
        if irq == 0 {
            windows_pic_debug_log(format!(
                "[PICQ] irq=0 mask=0x{:02x} irr=0x{:02x} in_service=0x{:02x} vector={:?}",
                pics.master.mask, pics.master.irr, pics.master.in_service, vector
            ));
        }
        vector
    } else if irq < 16 {
        if (pics.master.mask & (1 << 2)) != 0 {
            return None;
        }
        pics.slave.vector_for_irq(irq - 8)
    } else {
        None
    }
}

pub fn raise_irq(irq: u8) {
    if let Ok(mut pics) = global_pic_state().lock() {
        if irq < 8 {
            pics.master.raise_irq(irq);
            if irq == 0 {
                windows_pic_debug_log(format!(
                    "[PICR] irq=0 master_mask=0x{:02x} master_irr=0x{:02x} master_in_service=0x{:02x}",
                    pics.master.mask, pics.master.irr, pics.master.in_service
                ));
            }
        } else if irq < 16 {
            pics.slave.raise_irq(irq - 8);
            pics.master.raise_irq(2);
            windows_pic_debug_log(format!(
                "[PICR] irq={} master_irr=0x{:02x} slave_irr=0x{:02x} master_in_service=0x{:02x} slave_in_service=0x{:02x}",
                irq, pics.master.irr, pics.slave.irr, pics.master.in_service, pics.slave.in_service
            ));
        }
    }
}

pub fn acknowledge_irq(irq: u8) {
    if let Ok(mut pics) = global_pic_state().lock() {
        if irq < 8 {
            pics.master.mark_in_service(irq);
            windows_pic_debug_log(format!(
                "[PICA] irq={} master_mask=0x{:02x} master_irr=0x{:02x} master_in_service=0x{:02x}",
                irq, pics.master.mask, pics.master.irr, pics.master.in_service
            ));
        } else if irq < 16 {
            pics.master.mark_in_service(2);
            pics.slave.mark_in_service(irq - 8);
            windows_pic_debug_log(format!(
                "[PICA] irq={} master_irr=0x{:02x} slave_irr=0x{:02x} master_in_service=0x{:02x} slave_in_service=0x{:02x}",
                irq, pics.master.irr, pics.slave.irr, pics.master.in_service, pics.slave.in_service
            ));
        }
    }
}

pub struct PicStub {
    primary: bool,
}

impl PicStub {
    pub fn primary() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self { primary: true }))
    }

    pub fn secondary() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self { primary: false }))
    }

    fn name(&self) -> &'static str {
        if self.primary {
            "master"
        } else {
            "slave"
        }
    }
}

impl BusDevice for PicStub {
    fn read(&mut self, _vcpuid: u64, offset: u64, data: &mut [u8]) {
        if data.len() != 1 {
            return;
        }

        let value = if let Ok(pics) = global_pic_state().lock() {
            let state = if self.primary {
                &pics.master
            } else {
                &pics.slave
            };
            match offset {
                0 => state.read_command(),
                1 => state.read_data(),
                _ => 0,
            }
        } else {
            0
        };

        data[0] = value;
        windows_pic_debug_log(format!(
            "[PIC] {} read offset={} value=0x{:02x}",
            self.name(),
            offset,
            value
        ));
    }

    fn write(&mut self, _vcpuid: u64, offset: u64, data: &[u8]) {
        if data.len() != 1 {
            return;
        }

        if let Ok(mut pics) = global_pic_state().lock() {
            let state = if self.primary {
                &mut pics.master
            } else {
                &mut pics.slave
            };
            match offset {
                0 => state.handle_command_write(data[0], self.name()),
                1 => state.handle_data_write(data[0], self.name()),
                _ => {}
            }
        }
    }
}
