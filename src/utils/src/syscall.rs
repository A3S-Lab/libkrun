// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use std::os::raw::c_int;

/// Wrapper to interpret syscall exit codes and provide a rustacean `io::Result`
pub struct SyscallReturnCode(pub c_int);
impl SyscallReturnCode {
    /// Returns the last OS error if value is -1 or Ok(value) otherwise.
    pub fn into_result(self) -> std::io::Result<c_int> {
        if self.0 == -1 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(self.0)
        }
    }

    /// Returns the last OS error if value is -1 or Ok(()) otherwise.
    pub fn into_empty_result(self) -> std::io::Result<()> {
        self.into_result().map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_into_result_success() {
        let code = SyscallReturnCode(0);
        assert_eq!(code.into_result().unwrap(), 0);
    }

    #[test]
    fn test_into_result_positive() {
        let code = SyscallReturnCode(42);
        assert_eq!(code.into_result().unwrap(), 42);
    }

    #[test]
    fn test_into_result_negative_one() {
        let code = SyscallReturnCode(-1);
        assert!(code.into_result().is_err());
    }

    #[test]
    fn test_into_empty_result_success() {
        let code = SyscallReturnCode(0);
        assert!(code.into_empty_result().is_ok());
    }

    #[test]
    fn test_into_empty_result_error() {
        let code = SyscallReturnCode(-1);
        assert!(code.into_empty_result().is_err());
    }

    #[test]
    fn test_into_result_large_positive() {
        let code = SyscallReturnCode(i32::MAX);
        assert_eq!(code.into_result().unwrap(), i32::MAX);
    }

    #[test]
    fn test_into_result_large_negative() {
        let code = SyscallReturnCode(-100);
        assert!(code.into_result().is_err());
    }
}
