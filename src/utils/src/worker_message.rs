#[derive(Clone, Debug)]
pub struct MemoryProperties {
    pub gpa: u64,
    pub size: u64,
    pub private: bool,
}

#[derive(Debug)]
pub enum WorkerMessage {
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    GsiRoute(
        crossbeam_channel::Sender<bool>,
        Vec<kvm_bindings::kvm_irq_routing_entry>,
    ),
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    IrqLine(crossbeam_channel::Sender<bool>, u32, bool),
    #[cfg(target_os = "macos")]
    GpuAddMapping(crossbeam_channel::Sender<bool>, u64, u64, u64),
    #[cfg(target_os = "macos")]
    GpuRemoveMapping(crossbeam_channel::Sender<bool>, u64, u64),
    ConvertMemory(crossbeam_channel::Sender<bool>, MemoryProperties),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_properties_debug() {
        let props = MemoryProperties {
            gpa: 0x1000,
            size: 0x2000,
            private: true,
        };
        let debug_str = format!("{:?}", props);
        assert!(debug_str.contains("MemoryProperties"));
        assert!(debug_str.contains("0x1000"));
        assert!(debug_str.contains("0x2000"));
        assert!(debug_str.contains("private"));
    }

    #[test]
    fn test_memory_properties_clone() {
        let props = MemoryProperties {
            gpa: 0x1000,
            size: 0x2000,
            private: true,
        };
        let cloned = props.clone();
        assert_eq!(props.gpa, cloned.gpa);
        assert_eq!(props.size, cloned.size);
        assert_eq!(props.private, cloned.private);
    }

    #[test]
    fn test_memory_properties_default() {
        // Test with typical values
        let props = MemoryProperties {
            gpa: 0,
            size: 4096,
            private: false,
        };
        assert_eq!(props.gpa, 0);
        assert_eq!(props.size, 4096);
        assert!(!props.private);
    }

    #[test]
    fn test_worker_message_convert_memory() {
        // Test that ConvertMemory variant can be created and debugged
        // We use a disconnected channel for testing
        let (sender, _receiver) = crossbeam_channel::unbounded::<bool>();
        let props = MemoryProperties {
            gpa: 0x1000,
            size: 0x2000,
            private: false,
        };
        let msg = WorkerMessage::ConvertMemory(sender, props);
        let debug_str = format!("{:?}", msg);
        assert!(debug_str.contains("ConvertMemory"));
    }
}
