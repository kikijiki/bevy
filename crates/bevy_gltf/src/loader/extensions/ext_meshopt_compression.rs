//! Support for the EXT_meshopt_compression extension.
//! 
//! This extension allows compressing geometry using the meshoptimizer library
//! to reduce file sizes significantly.

use crate::GltfError;

/// Parsed data from the `EXT_meshopt_compression` extension.
///
/// See the specification:
/// <https://github.com/KhronosGroup/glTF/blob/main/extensions/2.0/Vendor/EXT_meshopt_compression/README.md>
#[derive(Default)]
pub(crate) struct MeshoptCompressionExtension;

impl MeshoptCompressionExtension {
    /// Decompresses buffer views that use EXT_meshopt_compression extension.
    /// This modifies the buffer data in-place, replacing compressed data with decompressed data.
    pub(crate) fn decompress_meshopt_buffer_views(
        document: &gltf::Document,
        buffer_data: &mut [Vec<u8>],
    ) -> Result<(), GltfError> {
        // First pass: calculate required buffer sizes for fallback buffers
        let mut required_buffer_sizes: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
        for view in document.views() {
            if let Some(_compression) = view.meshopt_compression() {
                let target_buffer_index = view.buffer().index();
                let target_offset = view.offset();
                let target_length = view.length();
                let required_size = target_offset + target_length;
                
                required_buffer_sizes
                    .entry(target_buffer_index)
                    .and_modify(|size| *size = (*size).max(required_size))
                    .or_insert(required_size);
            }
        }
        
        // Resize buffers to accommodate decompressed data
        for (buffer_index, required_size) in required_buffer_sizes {
            if buffer_data[buffer_index].len() < required_size {
                buffer_data[buffer_index].resize(required_size, 0);
            }
        }
        
        // Second pass: decompress and write data
        for view in document.views() {
            if let Some(compression) = view.meshopt_compression() {
                let buffer_index = compression.buffer.value();
                let byte_offset = compression.byte_offset.unwrap_or_default().0 as usize;
                let byte_length = compression.byte_length.0 as usize;
                
                // Get the compressed data from the source buffer
                let compressed_data = &buffer_data[buffer_index][byte_offset..byte_offset + byte_length];
                
                // Decompress based on mode and stride
                let count = compression.count as usize;
                let stride = compression.byte_stride as usize;
                
                // Validate parameters according to EXT_meshopt_compression specification
                Self::validate_compression_parameters(&compression, &view)?;
                
                let mut decompressed = Self::decompress_buffer_data(&compression, compressed_data, count, stride)?;
                
                // Apply filter if specified (using FFI directly)
                if let Some(filter) = &compression.filter {
                    Self::apply_filter(filter, &mut decompressed, stride)?;
                }
                
                // Replace the buffer view data with decompressed data
                let target_buffer_index = view.buffer().index();
                let target_offset = view.offset();
                let target_length = view.length();
                
                // Spec requires exact size match
                if decompressed.len() != target_length {
                    return Err(GltfError::Gltf(gltf::Error::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!(
                            "View {}: Decompressed size mismatch: got {} bytes, expected {} bytes",
                            view.index(),
                            decompressed.len(),
                            target_length
                        )
                    ))));
                }
                
                // Copy decompressed data to the target location (buffer was already resized in first pass)
                buffer_data[target_buffer_index][target_offset..target_offset + target_length]
                    .copy_from_slice(&decompressed);
            }
        }
        
        Ok(())
    }

    /// Validates compression parameters according to EXT_meshopt_compression specification
    fn validate_compression_parameters(
        compression: &gltf::json::extensions::buffer::MeshoptCompression,
        view: &gltf::buffer::View,
    ) -> Result<(), GltfError> {
        use gltf::json::extensions::buffer::{MeshoptCompressionMode, MeshoptCompressionFilter};
        
        let count = compression.count as usize;
        let stride = compression.byte_stride as usize;
        
        // Validate parent bufferView constraints from the spec:
        // "When parent bufferView has byteStride defined, it matches byteStride in the extension JSON"
        if let Some(view_stride) = view.stride() {
            if view_stride != stride {
                return Err(GltfError::Gltf(gltf::Error::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Parent bufferView byteStride ({}) does not match extension byteStride ({})", view_stride, stride)
                ))));
            }
        }
        
        // "The parent bufferView.byteLength is equal to byteStride times count"
        let expected_length = stride * count;
        if view.length() != expected_length {
            return Err(GltfError::Gltf(gltf::Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Parent bufferView byteLength ({}) does not equal byteStride * count ({})", view.length(), expected_length)
            ))));
        }
        
        match compression.mode {
            MeshoptCompressionMode::Attributes => {
                // When mode is "ATTRIBUTES", byteStride must be divisible by 4 and must be <= 256
                if stride % 4 != 0 {
                    return Err(GltfError::Gltf(gltf::Error::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("For ATTRIBUTES mode, byteStride must be divisible by 4, got {}", stride)
                    ))));
                }
                if stride > 256 {
                    return Err(GltfError::Gltf(gltf::Error::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("For ATTRIBUTES mode, byteStride must be <= 256, got {}", stride)
                    ))));
                }
            }
            MeshoptCompressionMode::Triangles => {
                // When mode is "TRIANGLES", count must be divisible by 3
                if count % 3 != 0 {
                    return Err(GltfError::Gltf(gltf::Error::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("For TRIANGLES mode, count must be divisible by 3, got {}", count)
                    ))));
                }
                // When mode is "TRIANGLES", byteStride must be equal to 2 or 4
                if stride != 2 && stride != 4 {
                    return Err(GltfError::Gltf(gltf::Error::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("For TRIANGLES mode, byteStride must be 2 or 4, got {}", stride)
                    ))));
                }
                // When mode is "TRIANGLES", filter must be equal to "NONE" or omitted
                if let Some(filter) = &compression.filter {
                    if !matches!(filter, MeshoptCompressionFilter::None) {
                        return Err(GltfError::Gltf(gltf::Error::Io(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "For TRIANGLES mode, filter must be NONE or omitted".to_string()
                        ))));
                    }
                }
            }
            MeshoptCompressionMode::Indices => {
                // When mode is "INDICES", byteStride must be equal to 2 or 4
                if stride != 2 && stride != 4 {
                    return Err(GltfError::Gltf(gltf::Error::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("For INDICES mode, byteStride must be 2 or 4, got {}", stride)
                    ))));
                }
                // When mode is "INDICES", filter must be equal to "NONE" or omitted
                if let Some(filter) = &compression.filter {
                    if !matches!(filter, MeshoptCompressionFilter::None) {
                        return Err(GltfError::Gltf(gltf::Error::Io(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "For INDICES mode, filter must be NONE or omitted".to_string()
                        ))));
                    }
                }
            }
        }
        
        // Validate filter-specific constraints
        if let Some(filter) = &compression.filter {
            match filter {
                MeshoptCompressionFilter::Octahedral => {
                    // When filter is "OCTAHEDRAL", byteStride must be equal to 4 or 8
                    if stride != 4 && stride != 8 {
                        return Err(GltfError::Gltf(gltf::Error::Io(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("For OCTAHEDRAL filter, byteStride must be 4 or 8, got {}", stride)
                        ))));
                    }
                }
                MeshoptCompressionFilter::Quaternion => {
                    // When filter is "QUATERNION", byteStride must be equal to 8
                    if stride != 8 {
                        return Err(GltfError::Gltf(gltf::Error::Io(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("For QUATERNION filter, byteStride must be 8, got {}", stride)
                        ))));
                    }
                }
                MeshoptCompressionFilter::Exponential => {
                    // When filter is "EXPONENTIAL", byteStride must be divisible by 4
                    if stride % 4 != 0 {
                        return Err(GltfError::Gltf(gltf::Error::Io(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("For EXPONENTIAL filter, byteStride must be divisible by 4, got {}", stride)
                        ))));
                    }
                }
                MeshoptCompressionFilter::None => {}
            }
        }

        Ok(())
    }

    /// Decompresses buffer data based on compression mode and parameters
    fn decompress_buffer_data(
        compression: &gltf::json::extensions::buffer::MeshoptCompression,
        compressed_data: &[u8],
        count: usize,
        stride: usize,
    ) -> Result<Vec<u8>, GltfError> {
        use gltf::json::extensions::buffer::MeshoptCompressionMode;
        
        match compression.mode {
            MeshoptCompressionMode::Attributes => {
                // For vertex attributes, use meshopt FFI directly to handle any valid stride
                let mut decompressed_vertices = vec![0u8; count * stride];
                
                // SAFETY: We're calling meshopt FFI functions on properly allocated buffers.
                // The decompressed_vertices buffer has been allocated with the correct size (count * stride).
                // The compressed_data comes from the glTF buffer and meshopt will validate it.
                // We've validated the stride according to the spec above.
                #[allow(unsafe_code)]
                unsafe {
                    let result = meshopt::ffi::meshopt_decodeVertexBuffer(
                        decompressed_vertices.as_mut_ptr() as *mut std::ffi::c_void,
                        count,
                        stride,
                        compressed_data.as_ptr(),
                        compressed_data.len(),
                    );
                    
                    if result != 0 {
                        return Err(GltfError::Gltf(gltf::Error::Io(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("Failed to decompress meshopt vertex buffer with stride {}: error code {}", stride, result)
                        ))));
                    }
                }
                
                Ok(decompressed_vertices)
            }
            MeshoptCompressionMode::Triangles | MeshoptCompressionMode::Indices => {
                // For indices, decode based on stride (2 or 4 bytes)
                match stride {
                    2 => {
                        let indices: Vec<u16> = meshopt::encoding::decode_index_buffer(compressed_data, count)
                            .map_err(|e| GltfError::Gltf(gltf::Error::Io(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!("Failed to decompress meshopt index buffer: {:?}", e)
                            ))))?;
                        Ok(indices.iter().flat_map(|&i| i.to_le_bytes()).collect())
                    }
                    4 => {
                        let indices: Vec<u32> = meshopt::encoding::decode_index_buffer(compressed_data, count)
                            .map_err(|e| GltfError::Gltf(gltf::Error::Io(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                format!("Failed to decompress meshopt index buffer: {:?}", e)
                            ))))?;
                        Ok(indices.iter().flat_map(|&i| i.to_le_bytes()).collect())
                    }
                    _ => Err(GltfError::Gltf(gltf::Error::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("Invalid stride {} for index buffer", stride)
                    )))),
                }
            }
        }
    }

    /// Applies meshopt filter to decompressed data
    fn apply_filter(
        filter: &gltf::json::extensions::buffer::MeshoptCompressionFilter,
        decompressed: &mut [u8],
        stride: usize,
    ) -> Result<(), GltfError> {
        use gltf::json::extensions::buffer::MeshoptCompressionFilter;
        use meshopt::ffi;
        
        let vertex_count = decompressed.len() / stride;
        
        // SAFETY: We're calling meshopt FFI functions on properly allocated and sized buffers.
        // The decompressed buffer has been allocated by meshopt and has the correct size.
        // The vertex_count and stride values come from the glTF extension and are validated
        // by the decompression functions above.
        #[allow(unsafe_code)]
        unsafe {
            match filter {
                MeshoptCompressionFilter::None => {}
                MeshoptCompressionFilter::Octahedral => {
                    ffi::meshopt_decodeFilterOct(
                        decompressed.as_mut_ptr() as *mut std::ffi::c_void,
                        vertex_count,
                        stride,
                    );
                }
                MeshoptCompressionFilter::Quaternion => {
                    ffi::meshopt_decodeFilterQuat(
                        decompressed.as_mut_ptr() as *mut std::ffi::c_void,
                        vertex_count,
                        stride,
                    );
                }
                MeshoptCompressionFilter::Exponential => {
                    ffi::meshopt_decodeFilterExp(
                        decompressed.as_mut_ptr() as *mut std::ffi::c_void,
                        vertex_count,
                        stride,
                    );
                }
            }
        }
        
        Ok(())
    }
}