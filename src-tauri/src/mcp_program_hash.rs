//! Streaming SHA-256 of an already pinned program handle. CNG keeps the
//! cryptographic loop outside unoptimized Rust without changing the digest,
//! byte budget, file lease, or permission/discovery deadlines.
use std::io::Read;

#[cfg(windows)]
mod cng {
    use std::{ffi::c_void, ptr};
    type Handle = *mut c_void;
    #[link(name = "bcrypt")]
    extern "system" {
        fn BCryptOpenAlgorithmProvider(
            out: *mut Handle,
            algorithm: *const u16,
            provider: *const u16,
            flags: u32,
        ) -> i32;
        fn BCryptCreateHash(
            algorithm: Handle,
            out: *mut Handle,
            object: *mut u8,
            object_len: u32,
            secret: *const u8,
            secret_len: u32,
            flags: u32,
        ) -> i32;
        fn BCryptHashData(hash: Handle, input: *const u8, length: u32, flags: u32) -> i32;
        fn BCryptFinishHash(hash: Handle, output: *mut u8, length: u32, flags: u32) -> i32;
        fn BCryptDestroyHash(hash: Handle) -> i32;
        fn BCryptCloseAlgorithmProvider(algorithm: Handle, flags: u32) -> i32;
    }
    pub(super) struct Hash {
        algorithm: Handle,
        hash: Handle,
    }
    impl Drop for Hash {
        fn drop(&mut self) {
            // CNG allocates the object for a null object buffer on Windows 7+;
            // DestroyHash owns that allocation. The provider outlives the hash.
            unsafe {
                if !self.hash.is_null() {
                    BCryptDestroyHash(self.hash);
                }
                if !self.algorithm.is_null() {
                    BCryptCloseAlgorithmProvider(self.algorithm, 0);
                }
            }
        }
    }
    impl Hash {
        pub(super) fn new() -> Result<Self, String> {
            let mut value = Self {
                algorithm: ptr::null_mut(),
                hash: ptr::null_mut(),
            };
            let sha256 = [83u16, 72, 65, 50, 53, 54, 0];
            // Every pointer refers to a live local buffer/handle. NTSTATUS is
            // signed; failure never falls back to a weaker or partial digest.
            unsafe {
                if BCryptOpenAlgorithmProvider(
                    &mut value.algorithm,
                    sha256.as_ptr(),
                    ptr::null(),
                    0,
                ) < 0
                    || value.algorithm.is_null()
                {
                    return Err("MCP_PROGRAM_HASH".into());
                }
                if BCryptCreateHash(
                    value.algorithm,
                    &mut value.hash,
                    ptr::null_mut(),
                    0,
                    ptr::null(),
                    0,
                    0,
                ) < 0
                    || value.hash.is_null()
                {
                    return Err("MCP_PROGRAM_HASH".into());
                }
            }
            Ok(value)
        }
        pub(super) fn update(&mut self, bytes: &[u8]) -> Result<(), String> {
            let size = u32::try_from(bytes.len()).map_err(|_| "MCP_PROGRAM_LIMIT")?;
            if unsafe { BCryptHashData(self.hash, bytes.as_ptr(), size, 0) } < 0 {
                return Err("MCP_PROGRAM_HASH".into());
            }
            Ok(())
        }
        pub(super) fn finish(self) -> Result<[u8; 32], String> {
            let mut output = [0u8; 32];
            if unsafe { BCryptFinishHash(self.hash, output.as_mut_ptr(), 32, 0) } < 0 {
                return Err("MCP_PROGRAM_HASH".into());
            }
            Ok(output)
        }
    }
}

pub(crate) fn hash(reader: impl Read, length: u64) -> Result<String, String> {
    if length > 256 * 1024 * 1024 {
        return Err("MCP_PROGRAM_LIMIT".into());
    }
    let mut reader = reader.take(length + 1);
    #[cfg(windows)]
    let mut hash = cng::Hash::new()?;
    #[cfg(not(windows))]
    let mut hash = {
        use sha2::Digest;
        sha2::Sha256::new()
    };
    let mut bytes = [0u8; 65536];
    let mut count = 0u64;
    loop {
        let n = reader.read(&mut bytes).map_err(|_| "MCP_PROGRAM_READ")?;
        if n == 0 {
            break;
        }
        count += n as u64;
        #[cfg(windows)]
        hash.update(&bytes[..n])?;
        #[cfg(not(windows))]
        {
            use sha2::Digest;
            hash.update(&bytes[..n]);
        }
    }
    if count != length {
        return Err("MCP_PROGRAM_CHANGED".into());
    }
    #[cfg(windows)]
    let output = hash.finish()?;
    #[cfg(not(windows))]
    let output = {
        use sha2::Digest;
        hash.finalize()
    };
    Ok(output.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::io::{self, Cursor};
    #[test]
    fn streaming_native_digest_matches_sha256_vectors_and_chunk_boundaries_and_denies_incomplete_reads(
    ) {
        assert_eq!(
            hash(Cursor::new(b""), 0).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hash(Cursor::new(b"abc"), 3).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        for n in [65535, 65536, 65537, 131071, 131072] {
            let bytes = (0..n).map(|i| (i % 251) as u8).collect::<Vec<_>>();
            assert_eq!(
                hash(Cursor::new(&bytes), n).unwrap(),
                format!("{:x}", Sha256::digest(&bytes))
            );
            assert_eq!(
                hash(Cursor::new(&bytes), n - 1).unwrap_err(),
                "MCP_PROGRAM_CHANGED"
            );
            assert_eq!(
                hash(Cursor::new(&bytes), n + 1).unwrap_err(),
                "MCP_PROGRAM_CHANGED"
            );
        }
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("owned read failure"))
            }
        }
        assert_eq!(hash(Broken, 1).unwrap_err(), "MCP_PROGRAM_READ");
        assert_eq!(
            hash(Broken, 256 * 1024 * 1024 + 1).unwrap_err(),
            "MCP_PROGRAM_LIMIT"
        );
    }
}
