//! Denormal sayıları önleme (flush-to-zero / denormals-are-zero).

/// Çağıran thread için FTZ ve DAZ bayraklarını açar. Ses thread'inde bir kez
/// çağrılması yeterlidir; diğer mimarilerde hiçbir şey yapmaz.
#[inline]
pub fn enable_flush_to_zero() {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: Yalnızca MXCSR'deki FTZ (bit 15) ve DAZ (bit 6) bayraklarını açar.
    unsafe {
        let mut csr: u32 = 0;
        std::arch::asm!("stmxcsr [{}]", in(reg) &mut csr, options(nostack));
        csr |= 0x8040;
        std::arch::asm!("ldmxcsr [{}]", in(reg) &csr, options(nostack));
    }
}
