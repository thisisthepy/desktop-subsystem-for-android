//! Presenting a task's frames through a `CAMetalLayer`.
//!
//! A frame arrives in a shared `IOSurface`. The surface is made a Metal texture once, and
//! each frame is copied from it into the layer's next drawable by a blit on the GPU and
//! presented, without passing through memory. Where the surface cannot be made a texture,
//! its BGRA pixels are read and written into a staging texture instead. The drawable takes
//! the size of the frame's content ([`super::sizing::place`]). The task's layer sits in a
//! root layer the window's view hosts, over the rectangle core places the frame in
//! ([`Presenter::place`]), and the root layer is painted the colour core gives the rest.

use std::ffi::c_void;

use objc2::encode::{Encode, Encoding, RefEncode};
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;

use super::sizing::place;
use super::surface::Surface;
use crate::objc::{class, ns_string, CGSize};

extern "C" {
    fn MTLCreateSystemDefaultDevice() -> *mut AnyObject;
}

/// `MTLPixelFormatBGRA8Unorm`.
const BGRA8_UNORM: usize = 80;
/// `MTLStorageModeManaged`: the CPU writes, the GPU reads; on every Mac.
const STORAGE_MANAGED: usize = 1;
/// `MTLTextureUsageShaderRead`.
const USAGE_SHADER_READ: usize = 1;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct MTLOrigin {
    x: usize,
    y: usize,
    z: usize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct MTLSize {
    width: usize,
    height: usize,
    depth: usize,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct MTLRegion {
    origin: MTLOrigin,
    size: MTLSize,
}

unsafe impl Encode for MTLOrigin {
    const ENCODING: Encoding = Encoding::Struct(
        "MTLOrigin",
        &[usize::ENCODING, usize::ENCODING, usize::ENCODING],
    );
}
unsafe impl RefEncode for MTLOrigin {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
}
unsafe impl Encode for MTLSize {
    const ENCODING: Encoding = Encoding::Struct(
        "MTLSize",
        &[usize::ENCODING, usize::ENCODING, usize::ENCODING],
    );
}
unsafe impl RefEncode for MTLSize {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
}
unsafe impl Encode for MTLRegion {
    const ENCODING: Encoding =
        Encoding::Struct("MTLRegion", &[MTLOrigin::ENCODING, MTLSize::ENCODING]);
}
unsafe impl RefEncode for MTLRegion {
    const ENCODING_REF: Encoding = Encoding::Pointer(&Self::ENCODING);
}

/// An `IOSurfaceRef`, encoded as the Metal method that takes one declares it.
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
struct IOSurfaceRef(*mut c_void);

unsafe impl Encode for IOSurfaceRef {
    const ENCODING: Encoding = Encoding::Pointer(&Encoding::Struct("__IOSurface", &[]));
}

/// One frame of a task: BGRA pixels, `stride` bytes a row, of which `content` pixels from the
/// top left are the task.
#[derive(Debug, Clone, Copy)]
pub struct FrameRef<'a> {
    pub pixels: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub content: (u32, u32),
}

/// The layer of one task window and what it takes to fill it.
pub struct Presenter {
    device: Retained<AnyObject>,
    queue: Retained<AnyObject>,
    /// The layer the window's view hosts: the colour under the frame.
    root: Retained<AnyObject>,
    /// The task's layer, over the rectangle the frame is placed in.
    layer: Retained<AnyObject>,
    staging: Option<(Retained<AnyObject>, (u32, u32))>,
    /// The texture over the frame surface last shown, by its texture ID.
    surface_texture: Option<(u64, Retained<AnyObject>)>,
    drawable: (u32, u32),
}

impl Presenter {
    /// A `CAMetalLayer` on the system's default GPU, set to show drawables one to one from
    /// the top left at the display's `scale`.
    pub fn new(scale: f64) -> Result<Presenter, String> {
        unsafe {
            let device = Retained::from_raw(MTLCreateSystemDefaultDevice())
                .ok_or("this Mac has no Metal device")?;
            let queue: Option<Retained<AnyObject>> = msg_send![&*device, newCommandQueue];
            let queue = queue.ok_or("the Metal device gave no command queue")?;
            let layer_class = class(c"CAMetalLayer")?;
            let layer: Option<Retained<AnyObject>> = msg_send![layer_class, layer];
            let layer = layer.ok_or("no CAMetalLayer could be made")?;
            let _: () = msg_send![&*layer, setDevice: &*device];
            let _: () = msg_send![&*layer, setPixelFormat: BGRA8_UNORM];
            // The blit writes into the drawable, which a framebuffer-only texture forbids.
            let _: () = msg_send![&*layer, setFramebufferOnly: false];
            let _: () = msg_send![&*layer, setContentsScale: scale];
            // The drawable fills the rectangle core places the frame in, which is the frame's
            // own size unless core scales it.
            let gravity = ns_string("resize");
            let _: () = msg_send![&*layer, setContentsGravity: &*gravity];
            let _: () = msg_send![&*layer, setOpaque: true];
            let root_class = class(c"CALayer")?;
            let root: Option<Retained<AnyObject>> = msg_send![root_class, layer];
            let root = root.ok_or("no CALayer could be made")?;
            // AppKit's layers are not flipped, so a frame placed from the top left is placed
            // from the visual top only once the root's geometry is flipped.
            let _: () = msg_send![&*root, setGeometryFlipped: true];
            let _: () = msg_send![&*root, setContentsScale: scale];
            let _: () = msg_send![&*root, addSublayer: &*layer];
            let _: () = msg_send![&*layer, setAllowsNextDrawableTimeout: false];
            Ok(Presenter {
                device,
                queue,
                root,
                layer,
                staging: None,
                surface_texture: None,
                drawable: (0, 0),
            })
        }
    }

    /// The root layer, for the window's content view to host.
    pub fn root_layer(&self) -> &AnyObject {
        &self.root
    }

    /// Puts the frame over `frame` (x, y, width, height in points from the content's top
    /// left) and paints the rest `fill` (0xAARRGGBB), or leaves it clear for `None`. Both
    /// change at once, without Core Animation's implicit animation.
    pub fn place(&self, (x, y, width, height): (f64, f64, f64, f64), fill: Option<u32>) {
        unsafe {
            let Ok(transactions) = class(c"CATransaction") else {
                return;
            };
            let _: () = msg_send![transactions, begin];
            let _: () = msg_send![transactions, setDisableActions: true];
            let frame = crate::objc::CGRect {
                origin: crate::objc::CGPoint { x, y },
                size: CGSize { width, height },
            };
            let _: () = msg_send![&*self.layer, setFrame: frame];
            let mut colour: *const c_void = std::ptr::null();
            if let (Some(fill), Ok(colors)) = (fill, class(c"NSColor")) {
                let channel = |shift: u32| f64::from((fill >> shift) & 0xFF) / 255.0;
                let color: *mut AnyObject = msg_send![
                    colors,
                    colorWithSRGBRed: channel(16),
                    green: channel(8),
                    blue: channel(0),
                    alpha: 1.0f64
                ];
                if !color.is_null() {
                    colour = msg_send![color, CGColor];
                }
            }
            let _: () = msg_send![&*self.root, setBackgroundColor: colour];
            let _: () = msg_send![transactions, commit];
        }
    }

    /// The display's backing scale changed (the window moved to another display).
    pub fn set_scale(&mut self, scale: f64) {
        unsafe {
            let _: () = msg_send![&*self.layer, setContentsScale: scale];
            let _: () = msg_send![&*self.root, setContentsScale: scale];
        }
    }

    fn staging(&mut self, size: (u32, u32)) -> Result<&AnyObject, String> {
        let fits = matches!(&self.staging, Some((_, have)) if *have == size);
        if !fits {
            unsafe {
                let descriptor_class = class(c"MTLTextureDescriptor")?;
                let descriptor: Option<Retained<AnyObject>> = msg_send![
                    descriptor_class,
                    texture2DDescriptorWithPixelFormat: BGRA8_UNORM,
                    width: size.0 as usize,
                    height: size.1 as usize,
                    mipmapped: false
                ];
                let descriptor = descriptor.ok_or("no texture descriptor")?;
                let _: () = msg_send![&*descriptor, setStorageMode: STORAGE_MANAGED];
                let _: () = msg_send![&*descriptor, setUsage: USAGE_SHADER_READ];
                let texture: Option<Retained<AnyObject>> =
                    msg_send![&*self.device, newTextureWithDescriptor: &*descriptor];
                let texture = texture.ok_or_else(|| {
                    format!("the GPU could not make a {}x{} texture", size.0, size.1)
                })?;
                self.staging = Some((texture, size));
            }
        }
        Ok(&self.staging.as_ref().expect("made above").0)
    }

    /// The Metal texture over `surface`, made once for texture `texture_id`.
    fn surface_texture(
        &mut self,
        texture_id: u64,
        surface: &Surface,
    ) -> Result<*const AnyObject, String> {
        if let Some((id, texture)) = &self.surface_texture {
            if *id == texture_id {
                return Ok(Retained::as_ptr(texture));
            }
        }
        self.surface_texture = None;
        let (width, height) = surface.size();
        unsafe {
            let descriptor_class = class(c"MTLTextureDescriptor")?;
            let descriptor: Option<Retained<AnyObject>> = msg_send![
                descriptor_class,
                texture2DDescriptorWithPixelFormat: BGRA8_UNORM,
                width: width as usize,
                height: height as usize,
                mipmapped: false
            ];
            let descriptor = descriptor.ok_or("no texture descriptor")?;
            let _: () = msg_send![&*descriptor, setUsage: USAGE_SHADER_READ];
            let texture: Option<Retained<AnyObject>> = msg_send![
                &*self.device,
                newTextureWithDescriptor: &*descriptor,
                iosurface: IOSurfaceRef(surface.as_raw()),
                plane: 0usize
            ];
            let texture = texture.ok_or("the GPU cannot show the frame surface directly")?;
            let pointer = Retained::as_ptr(&texture);
            self.surface_texture = Some((texture_id, texture));
            Ok(pointer)
        }
    }

    /// Shows the frame in `surface` on the GPU: `content` pixels from its top left. Where the
    /// GPU cannot take the surface as a texture, its pixels are read instead.
    pub fn present_surface(
        &mut self,
        texture_id: u64,
        surface: &Surface,
        content: (u32, u32),
    ) -> Result<(), String> {
        let buffer = surface.size();
        let source = match self.surface_texture(texture_id, surface) {
            Ok(source) => source,
            Err(_) => {
                return surface.read(|pixels, stride| {
                    self.present(FrameRef {
                        pixels,
                        width: buffer.0,
                        height: buffer.1,
                        stride: stride as u32,
                        content,
                    })
                })?;
            }
        };
        let Some(placement) = place(content, buffer) else {
            return Ok(());
        };
        let (width, height) = placement.copy;
        let size = MTLSize {
            width: width as usize,
            height: height as usize,
            depth: 1,
        };
        unsafe { self.blit(source, size, placement.drawable) }
    }

    /// Copies `size` from the top left of `source` into the next drawable, made `drawable`
    /// big, and presents it.
    unsafe fn blit(
        &mut self,
        source: *const AnyObject,
        size: MTLSize,
        drawable_size: (u32, u32),
    ) -> Result<(), String> {
        if self.drawable != drawable_size {
            let size = CGSize {
                width: f64::from(drawable_size.0),
                height: f64::from(drawable_size.1),
            };
            let _: () = msg_send![&*self.layer, setDrawableSize: size];
            self.drawable = drawable_size;
        }
        let drawable: Option<Retained<AnyObject>> = msg_send![&*self.layer, nextDrawable];
        let Some(drawable) = drawable else {
            // The window is hidden or off screen: nothing to show into.
            return Ok(());
        };
        let target: *mut AnyObject = msg_send![&*drawable, texture];
        let buffer: Option<Retained<AnyObject>> = msg_send![&*self.queue, commandBuffer];
        let buffer = buffer.ok_or("the GPU gave no command buffer")?;
        let blit: Option<Retained<AnyObject>> = msg_send![&*buffer, blitCommandEncoder];
        let blit = blit.ok_or("the GPU gave no blit encoder")?;
        let _: () = msg_send![
            &*blit,
            copyFromTexture: source,
            sourceSlice: 0usize,
            sourceLevel: 0usize,
            sourceOrigin: MTLOrigin::default(),
            sourceSize: size,
            toTexture: target,
            destinationSlice: 0usize,
            destinationLevel: 0usize,
            destinationOrigin: MTLOrigin::default()
        ];
        let _: () = msg_send![&*blit, endEncoding];
        let _: () = msg_send![&*buffer, presentDrawable: &*drawable];
        let _: () = msg_send![&*buffer, commit];
        // The window manager writes the surface again once the window host answers, so the
        // copy out of it must be done by then. A copy of one frame takes well under a
        // millisecond.
        let _: () = msg_send![&*buffer, waitUntilCompleted];
        Ok(())
    }

    /// Shows `frame`. A frame that is not whole (its pixels are fewer than its size says) is
    /// refused rather than read past its end.
    pub fn present(&mut self, frame: FrameRef<'_>) -> Result<(), String> {
        let needed = (frame.stride as usize).checked_mul(frame.height as usize);
        if frame.stride < frame.width.saturating_mul(4)
            || needed.is_none_or(|n| n > frame.pixels.len())
        {
            return Err(format!(
                "a {}x{} frame with {} bytes a row does not fit its {} bytes",
                frame.width,
                frame.height,
                frame.stride,
                frame.pixels.len()
            ));
        }
        let Some(placement) = place(frame.content, (frame.width, frame.height)) else {
            return Ok(());
        };
        let (width, height) = placement.copy;
        unsafe {
            if self.drawable != placement.drawable {
                let size = CGSize {
                    width: f64::from(placement.drawable.0),
                    height: f64::from(placement.drawable.1),
                };
                let _: () = msg_send![&*self.layer, setDrawableSize: size];
                self.drawable = placement.drawable;
            }
            let region = MTLRegion {
                origin: MTLOrigin::default(),
                size: MTLSize {
                    width: width as usize,
                    height: height as usize,
                    depth: 1,
                },
            };
            let staging: *const AnyObject = self.staging((width, height))?;
            let _: () = msg_send![
                staging,
                replaceRegion: region,
                mipmapLevel: 0usize,
                withBytes: frame.pixels.as_ptr() as *const c_void,
                bytesPerRow: frame.stride as usize
            ];
            let drawable: Option<Retained<AnyObject>> = msg_send![&*self.layer, nextDrawable];
            let Some(drawable) = drawable else {
                // The window is hidden or off screen: nothing to show into.
                return Ok(());
            };
            let target: *mut AnyObject = msg_send![&*drawable, texture];
            let buffer: Option<Retained<AnyObject>> = msg_send![&*self.queue, commandBuffer];
            let buffer = buffer.ok_or("the GPU gave no command buffer")?;
            let blit: Option<Retained<AnyObject>> = msg_send![&*buffer, blitCommandEncoder];
            let blit = blit.ok_or("the GPU gave no blit encoder")?;
            let _: () = msg_send![
                &*blit,
                copyFromTexture: staging,
                sourceSlice: 0usize,
                sourceLevel: 0usize,
                sourceOrigin: MTLOrigin::default(),
                sourceSize: region.size,
                toTexture: target,
                destinationSlice: 0usize,
                destinationLevel: 0usize,
                destinationOrigin: MTLOrigin::default()
            ];
            let _: () = msg_send![&*blit, endEncoding];
            let _: () = msg_send![&*buffer, presentDrawable: &*drawable];
            let _: () = msg_send![&*buffer, commit];
        }
        Ok(())
    }
}
