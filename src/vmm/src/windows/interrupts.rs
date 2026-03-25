// Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug)]
pub enum PendingInterrupt {
    PicExtInt { irq: u8, vector: u8 },
    PicFixed { irq: u8, vector: u8 },
}

pub type PendingInterruptQueue = Arc<Mutex<VecDeque<PendingInterrupt>>>;
