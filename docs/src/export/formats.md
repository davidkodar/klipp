# Export Formats

Detailed reference for all supported export containers, video codecs, and audio codecs.

## Containers

### MP4 (MPEG-4 Part 14)
The most widely supported container format. Compatible with virtually all media players, web browsers, and devices. Supports H.264, H.265, and AAC audio. Use MP4 for web delivery, YouTube, and general distribution.

### MKV (Matroska)
A flexible open-source container that supports nearly all video and audio codecs. MKV handles H.264, H.265, ProRes, DNxHD, FLAC, and many more. Ideal for archival and when codec flexibility is needed. Some devices and platforms have limited MKV support.

### MOV (QuickTime)
Apple's container format, required for ProRes output. Also supports H.264, H.265, and DNxHD. Use MOV when delivering ProRes masters or working with Apple-based post-production pipelines.

## Video Codecs

### H.264 / AVC
- **Use case**: Universal delivery, streaming, web
- **Quality**: Good at moderate bitrates
- **Encoding speed**: Fast
- **File size**: Moderate
- **Compatibility**: Plays everywhere

### H.265 / HEVC
- **Use case**: 4K delivery, bandwidth-constrained distribution
- **Quality**: Better than H.264 at the same bitrate (30-50% smaller files)
- **Encoding speed**: Slower than H.264
- **File size**: Small
- **Compatibility**: Most modern devices and players

### ProRes 422
- **Use case**: Post-production intermediate, delivery masters
- **Quality**: Visually lossless
- **Encoding speed**: Fast
- **File size**: Large (approximately 10x H.264 at similar quality)
- **Container**: MOV required

### ProRes 422 HQ
- **Use case**: High-end post-production, broadcast delivery
- **Quality**: Higher bitrate than standard 422
- **File size**: Very large
- **Container**: MOV required

### ProRes 4444
- **Use case**: VFX, compositing, alpha channel workflows
- **Quality**: Near-lossless with alpha channel support
- **File size**: Very large
- **Container**: MOV required

### DNxHD
- **Use case**: Avid Media Composer interchange, broadcast
- **Quality**: Visually lossless
- **Encoding speed**: Fast
- **File size**: Large
- **Container**: MOV or MKV

## Audio Codecs

### AAC (Advanced Audio Coding)
- **Use case**: Standard delivery, streaming
- **Quality**: Good at 128-256 kbps
- **Compatibility**: Universal
- **Container**: MP4, MOV, MKV

### MP3 (MPEG Layer 3)
- **Use case**: Legacy compatibility
- **Quality**: Acceptable at 192-320 kbps
- **Compatibility**: Universal
- **Container**: MP4, MKV

### FLAC (Free Lossless Audio Codec)
- **Use case**: Archival, lossless delivery
- **Quality**: Perfect (lossless)
- **File size**: 50-70% of uncompressed PCM
- **Container**: MKV
