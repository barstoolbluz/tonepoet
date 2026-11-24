//! Image editor processing. One geometry plan drives the form, preview and writes.
//! The editor never uses the viewer's downsampled pixels as source material.
use image::{DynamicImage, ImageDecoder, ImageEncoder, ImageFormat, ImageReader, Limits, Rgba, RgbaImage};
use image::imageops;
use lofty::{file::{AudioFile, TaggedFileExt}, picture::{MimeType, Picture, PictureType}, tag::TagType};
use sha2::{Digest, Sha256};
use std::{fs, io::{Cursor, Read, Write}, path::{Path, PathBuf}, sync::Arc};

const MAX_BYTES: u64 = 100 * 1024 * 1024;
// The viewer admits a 512 MiB decoded raster. Keep that input policy, and
// admit the editor's actual working set separately. Resampling holds only
// the smaller separable intermediate, never a source-sized f32 image.
const MAX_RASTER_BYTES: u64 = 512 * 1024 * 1024;
const MAX_WORK_BYTES: u64 = 768 * 1024 * 1024;
const MAX_DIM: u32 = 32768;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Format { Same, Jpeg, Png, Webp, Gif, Bmp }
impl Format {
    pub const ALL: [Self; 6] = [Self::Same, Self::Jpeg, Self::Png, Self::Webp, Self::Gif, Self::Bmp];
    pub fn label(self) -> &'static str { match self { Self::Same => "Same as source", Self::Jpeg => "JPEG", Self::Png => "PNG", Self::Webp => "WebP · lossless", Self::Gif => "GIF", Self::Bmp => "BMP" } }
    pub fn short(self) -> &'static str { if self == Self::Webp { "WebP" } else { self.label() } }
    pub fn extension(self) -> &'static str { match self { Self::Same => "", Self::Jpeg => "jpg", Self::Png => "png", Self::Webp => "webp", Self::Gif => "gif", Self::Bmp => "bmp" } }
    pub fn from_image(f: ImageFormat) -> Option<Self> { match f { ImageFormat::Jpeg => Some(Self::Jpeg), ImageFormat::Png => Some(Self::Png), ImageFormat::WebP => Some(Self::Webp), ImageFormat::Gif => Some(Self::Gif), ImageFormat::Bmp => Some(Self::Bmp), _ => None } }
    fn image(self) -> ImageFormat { match self { Self::Jpeg => ImageFormat::Jpeg, Self::Png => ImageFormat::Png, Self::Webp => ImageFormat::WebP, Self::Gif => ImageFormat::Gif, Self::Bmp => ImageFormat::Bmp, Self::Same => unreachable!("resolve first") } }
    pub(super) fn alpha(self) -> bool { matches!(self, Self::Png | Self::Webp | Self::Gif) }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode { Off, Scale, Percent, Long, Size }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fill { Crop, Pad, Stretch }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Square { Off, Crop, Pad }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Metadata { Strip, Icc, All }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WriteMode { File, Embed, Both }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Conflict { Ask, Replace, Skip, Number }

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Settings {
    pub mode: Mode, pub scale: f64, pub percent: f64, pub long: u32,
    pub width: u32, pub height: u32,
    // Source, 5:4, 4:3, 3:2, 16:10, 16:9, 21:9, Custom, Free.
    pub ratio: usize, pub custom_ratio: (u32, u32), pub portrait: bool,
    pub fill: Fill, pub filter: usize, pub linear: bool, pub no_upscale: bool,
    pub rotation: usize, pub flip_h: bool, pub flip_v: bool, pub square: Square,
    pub pad: Option<[u8; 4]>, pub format: Format, pub quality: u8,
    pub metadata: Metadata, pub write: WriteMode, pub conflict: Conflict, pub template: String,
}
impl Settings {
    pub fn new(embedded: bool) -> Self { Self {
        mode: Mode::Long, scale: 2.0, percent: 50.0, long: 1080, width: 1080, height: 1080,
        ratio: 0, custom_ratio: (1, 1), portrait: false, fill: Fill::Crop, filter: 0,
        linear: true, no_upscale: true, rotation: 0, flip_h: false, flip_v: false,
        square: Square::Off, pad: None, format: if embedded { Format::Jpeg } else { Format::Same },
        quality: 90, metadata: if embedded { Metadata::Strip } else { Metadata::Icc },
        write: WriteMode::File, conflict: Conflict::Ask,
        template: if embedded { "%ALBUMDIR%/cover.%EXT%" } else { "%DIR%/%NAME%_%W%x%H%.%EXT%" }.into(),
    } }
    pub fn ratio_terms(&self) -> Option<(u32,u32)> {
        let (a,b) = match self.ratio { 1 => (5,4), 2 => (4,3), 3 => (3,2), 4 => (16,10), 5 => (16,9), 6 => (21,9), 7 => self.custom_ratio, _ => return None };
        Some(if self.portrait { (b,a) } else { (a,b) })
    }
    pub fn resolved_format(&self, source: Format) -> Format { if self.format == Format::Same { source } else { self.format } }
    pub(super) fn pad_color(&self, format: Format) -> Rgba<u8> { let mut p = self.pad.unwrap_or(if format.alpha() { [0,0,0,0] } else { [0,0,0,255] }); if !format.alpha() { p[3] = 255; } Rgba(p) }
}

pub(super) fn snap8(x: f64) -> u32 { (8.0 * (x / 8.0 + 0.5).floor()).max(8.0).min(f64::from(u32::MAX - 7)) as u32 }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Geometry { pub transformed: (u32,u32), pub target: (u32,u32), pub resample: (u32,u32), pub kept: bool, pub fill: Fill }
impl Geometry {
    pub fn note(self) -> String {
        let (w,h) = self.target; let (rw,rh) = self.resample;
        match self.fill {
            Fill::Stretch => "distorts the image".into(),
            Fill::Crop => if rw > w { format!("trims {} px left and right", (rw-w)/2) } else { format!("trims {} px top and bottom", rh.saturating_sub(h)/2) },
            Fill::Pad => if w > rw { format!("adds {} px left and right", (w-rw)/2) } else { format!("adds {} px top and bottom", h.saturating_sub(rh)/2) },
        }
    }
}
pub(super) fn geometry(s: &Settings, original: (u32,u32)) -> Geometry {
    let (mut sw,mut sh) = original;
    if s.rotation % 2 == 1 { std::mem::swap(&mut sw,&mut sh); }
    match s.square { Square::Off => {}, Square::Crop => { sw=sw.min(sh); sh=sw; }, Square::Pad => { sw=sw.max(sh); sh=sw; } }
    let (a,b)=(f64::from(sw.max(1)), f64::from(sh.max(1)));
    let (w,h) = match s.mode {
        Mode::Off => (sw,sh), Mode::Scale => (snap8(a*s.scale),snap8(b*s.scale)),
        Mode::Percent => (snap8(a*s.percent/100.),snap8(b*s.percent/100.)),
        Mode::Long => { let k=f64::from(s.long)/a.max(b); (snap8(a*k),snap8(b*k)) },
        Mode::Size => { let w=snap8(f64::from(s.width)); let h=match s.ratio_terms() { Some((p,q)) => snap8(f64::from(w)*f64::from(q)/f64::from(p.max(1))), None if s.ratio==0 => snap8(f64::from(w)*b/a), _=>snap8(f64::from(s.height)) }; (w,h) },
    };
    let (x,y)=(f64::from(w)/a,f64::from(h)/b);
    let fill = if s.mode == Mode::Size && s.ratio != 0 && (x/y-1.).abs()>0.005 { s.fill } else { Fill::Stretch };
    let factor=match fill { Fill::Crop=>x.max(y), Fill::Pad=>x.min(y), Fill::Stretch=>x.max(y) };
    let kept = s.mode != Mode::Off && s.mode != Mode::Scale && s.no_upscale && factor > 1.;
    if kept || s.mode == Mode::Off { return Geometry { transformed:(sw,sh),target:(sw,sh),resample:(sw,sh),kept,fill }; }
    let resample=match fill { Fill::Stretch => (w,h), Fill::Crop => ((a*factor).round().max(f64::from(w)) as u32,(b*factor).round().max(f64::from(h)) as u32), Fill::Pad => ((a*factor).round().clamp(1.,f64::from(w)) as u32,(b*factor).round().clamp(1.,f64::from(h)) as u32) };
    Geometry { transformed:(sw,sh),target:(w,h),resample,kept,fill }
}
fn area((w,h):(u32,u32)) -> u64 { u64::from(w)*u64::from(h) }
fn admit((w,h):(u32,u32), bytes_per_pixel:u64) -> Result<(),String> {
    if w==0 || h==0 || w>MAX_DIM || h>MAX_DIM || area((w,h))*bytes_per_pixel>MAX_RASTER_BYTES {
        Err("Image exceeds the editor's raster-memory limit".into())
    } else { Ok(()) }
}
fn intermediate_area(from:(u32,u32),to:(u32,u32))->u64 {
    area((from.0,to.1)).min(area((to.0,from.1)))
}
fn admit_work(input:&Decoded, plan:Geometry, bpp:u64, s:&Settings,gray:bool)->Result<(),String> {
    let source=(input.pixels.width(),input.pixels.height());
    for d in [source,plan.transformed,plan.target,plan.resample] { admit(d,bpp)?; }
    let retained=area(source)*u64::from(input.pixels.color().bytes_per_pixel());
    let transform=(area(source)+area(plan.transformed).max(area(source)))*bpp;
    let resizing=plan.transformed!=plan.resample;
    let intermediate=if resizing&&s.filter!=2 { intermediate_area(plan.transformed,plan.resample)*16 } else {0};
    let resize=(area(plan.transformed)+area(plan.resample))*bpp+intermediate;
    let center=(area(plan.resample)+area(plan.target))*bpp;
    // Include an encoded-output allowance and the JPEG/BMP flatten buffer.
    let encode_extra=if gray&&bpp==8{4}else{3};
    let encode=area(plan.target)*(bpp+encode_extra)+MAX_BYTES;
    if retained+transform.max(resize).max(center).max(encode)>MAX_WORK_BYTES {
        return Err("Image operation exceeds the editor's working-memory limit; choose a smaller target".into());
    }
    Ok(())
}
fn err(e: impl std::fmt::Display) -> String { e.to_string() }
fn read_file(path: &Path) -> Result<Vec<u8>,String> {
    if !fs::metadata(path).map_err(err)?.is_file() { return Err("Not a regular image file".into()); }
    let file=fs::File::open(path).map_err(err)?;
    let opened=file.metadata().map_err(err)?;
    if !opened.is_file() || opened.len()>MAX_BYTES {return Err("Image source changed or exceeds 100 MiB".into());}
    let mut bytes=Vec::new(); file.take(MAX_BYTES+1).read_to_end(&mut bytes).map_err(err)?;
    if bytes.len() as u64>MAX_BYTES { return Err("Image exceeds 100 MiB encoded-size limit".into()); } Ok(bytes)
}
/// Header admission for the context menu; full validation happens in the worker.
/// Do not use a suffix or read a whole file on the menu/input thread.
pub(super) fn supported_file(path:&Path)->bool {
    if !fs::metadata(path).is_ok_and(|m| m.is_file() && m.len()<=MAX_BYTES) { return false; }
    let Ok(mut file)=fs::File::open(path) else { return false; };
    let mut head=[0u8;32]; let Ok(n)=file.read(&mut head) else { return false; };
    image::guess_format(&head[..n]).ok().and_then(Format::from_image).is_some()
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Source { File(PathBuf), Picture { path:PathBuf, tag:Option<TagType>, index:usize, kind:PictureType, description:Option<String>, digest:[u8;32], album:String, artist:String } }
impl Source {
    pub fn path(&self)->&Path { match self { Self::File(p)|Self::Picture{path:p,..}=>p } }
    pub fn label(&self)->String { match self { Self::File(p)=>p.file_name().unwrap_or_default().to_string_lossy().into_owned(), Self::Picture{kind,..}=>picture_label(*kind).into() } }
    pub fn embedded(&self)->bool { matches!(self,Self::Picture{..}) }
    pub fn can_reembed(&self)->bool { matches!(self,Self::Picture{tag:Some(_),..}) }
    pub fn bytes(&self)->Result<Vec<u8>,String> { match self { Self::File(p)=>read_file(p), Self::Picture{path,tag,index,kind,description,digest,..}=>{
        let (pictures,_,_)=super::probe::editor_pictures(path)?;
        let picture=pictures.into_iter().find(|p|p.tag==*tag && p.index==*index).ok_or("The embedded picture disappeared")?.picture;
        if fingerprint(picture.data())!=*digest || picture.pic_type()!=*kind || picture.description()!=description.as_deref() { return Err("The embedded picture changed; reopen Edit Image".into()); }
        if picture.data().len() as u64>MAX_BYTES { return Err("Embedded picture exceeds 100 MiB".into()); }
        Ok(picture.data().to_vec())
    } } }
}
pub(super) fn source_matches(source:&Source,digest:[u8;32])->Result<bool,String>{Ok(fingerprint(&source.bytes()?)==digest)}
fn fingerprint(bytes:&[u8])->[u8;32] { Sha256::digest(bytes).into() }
fn picture_label(kind:PictureType)->&'static str { match kind { PictureType::CoverFront=>"Front cover", PictureType::CoverBack=>"Back cover", PictureType::Other=>"Other", _=>"Embedded picture" } }
pub(super) fn pictures(path:&Path)->Result<(Vec<Source>,usize),String> {
    let (pictures,album,artist)=super::probe::editor_pictures(path)?;
    let mut out=Vec::new();
    for p in pictures {
        let pic=p.picture;
        if image::guess_format(pic.data()).ok().and_then(Format::from_image).is_some() {
            out.push(Source::Picture { path:path.to_path_buf(),tag:p.tag,index:p.index,kind:pic.pic_type(),description:pic.description().map(str::to_owned),digest:fingerprint(pic.data()),album:album.clone(),artist:artist.clone() });
        }
    }
    if out.is_empty() { return Err("No supported embedded pictures".into()); }
    // Match the viewer's preference, including the first picture within a type.
    let index=out.iter().position(|p|matches!(p,Source::Picture{kind:PictureType::CoverFront,..}))
        .or_else(||out.iter().position(|p|matches!(p,Source::Picture{kind:PictureType::Other,..}))).unwrap_or(0);
    Ok((out,index))
}
// Common descriptive metadata has standard containers in JPEG, PNG and
// WebP. Keep opaque, safe-to-copy records only in their source format; an
// unsupported representation is an explicit form error, never a silent drop.
const XMP_JPEG: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const XMP_PNG: &[u8] = b"XML:com.adobe.xmp";
#[derive(Clone, Debug, Default)]
struct ExtraMetadata {
    records: Vec<Vec<u8>>,
    descriptive: Vec<PngDescription>,
    color: Vec<Vec<u8>>,
    comments: Vec<Vec<u8>>,
    xmp: Option<Vec<u8>>,
    exif: Option<Vec<u8>>,
    icc: Option<Vec<u8>>,
    density: Option<(u32,u32,bool)>, // pixels/metre, or unitless pixel aspect
    unsupported: Option<String>,
}
#[derive(Clone, Debug)]
struct PngDescription { property: &'static str, value: String, raw: Vec<u8> }
impl ExtraMetadata {
    fn refuse(&mut self, what: &str) { if self.unsupported.is_none() {self.unsupported=Some(what.into());} }
    fn set_xmp(&mut self, data:&[u8])->Result<(),String> {
        if self.xmp.is_some() {self.refuse("multiple or extended XMP packets");return Ok(());}
        self.xmp=Some(data.to_vec());Ok(())
    }
}
fn be32(b:&[u8])->u32 {u32::from_be_bytes(b.try_into().unwrap())}
fn le32(b:&[u8])->u32 {u32::from_le_bytes(b.try_into().unwrap())}
fn zlib_text(bytes:&[u8])->Result<Vec<u8>,String> {
    let mut decoded=Vec::new();flate2::read::ZlibDecoder::new(bytes).take(16*1024*1024+1).read_to_end(&mut decoded).map_err(err)?;
    if decoded.len()>16*1024*1024 {return Err("Compressed PNG metadata exceeds 16 MiB".into());}Ok(decoded)
}
fn png_text<'a>(kind:&[u8],body:&'a [u8])->Result<(&'a [u8],Vec<u8>),String> {
    let end=body.iter().position(|b|*b==0).ok_or("PNG text has no keyword terminator")?;
    let key=&body[..end];let rest=&body[end+1..];
    let value=match kind {
        b"tEXt"=>rest.to_vec(),
        b"zTXt"=>{if rest.first()!=Some(&0){return Err("Invalid PNG text compression".into());}zlib_text(&rest[1..])?},
        b"iTXt"=>{
            if rest.len()<2 || rest[0]>1 || rest[1]!=0{return Err("Invalid PNG international text".into());}
            let strings=&rest[2..];let a=strings.iter().position(|b|*b==0).ok_or("Invalid PNG text language")?;
            let strings=&strings[a+1..];let b=strings.iter().position(|b|*b==0).ok_or("Invalid PNG translated keyword")?;
            if rest[0]==1{zlib_text(&strings[b+1..])?}else{strings[b+1..].to_vec()}
        },
        _=>return Err("Unexpected PNG text type".into()),
    };Ok((key,value))
}
fn container_metadata(bytes: &[u8], format: Format) -> Result<ExtraMetadata, String> {
    let mut extra = ExtraMetadata::default();
    match format {
        Format::Jpeg => {
            let mut pos = 2;
            while pos + 4 <= bytes.len() && bytes[pos] == 0xff {
                let start = pos;
                while bytes.get(pos) == Some(&0xff) { pos += 1; }
                let Some(&marker) = bytes.get(pos) else { break; };
                pos += 1;
                if marker == 0xda || marker == 0xd9 { break; }
                let Some(length) = bytes.get(pos..pos+2) else { return Err("Truncated JPEG segment".into()); };
                let length = usize::from(u16::from_be_bytes([length[0], length[1]]));
                if length < 2 || pos+length > bytes.len() { return Err("Invalid JPEG segment length".into()); }
                let body = &bytes[pos+2..pos+length];
                match marker {
                    0xe0 if body.starts_with(b"JFIF\0") && body.len()>=14=>{
                        let x=u32::from(u16::from_be_bytes([body[8],body[9]]));let y=u32::from(u16::from_be_bytes([body[10],body[11]]));
                        extra.density=match body[7] {0 if x!=y=>Some((x,y,false)),1=>Some(((f64::from(x)/0.0254).round() as u32,(f64::from(y)/0.0254).round() as u32,true)),2=>Some((x*100,y*100,true)),_=>None};
                        // JFIF's thumbnail is derived raster data; the encoder
                        // emits its own JFIF header without the stale thumbnail.
                    },
                    0xe1 if body.starts_with(b"Exif\0\0")=>{if extra.exif.is_some(){extra.refuse("multiple EXIF segments");}else{extra.exif=Some(body[6..].to_vec());}},
                    0xe1 if body.starts_with(XMP_JPEG)=>extra.set_xmp(&body[XMP_JPEG.len()..])?,
                    0xe2 if body.starts_with(b"ICC_PROFILE\0")=>{},
                    0xe0 if body.starts_with(b"JFXX\0")=>{}, // obsolete derived thumbnail
                    0xee=>{}, // Adobe transform marker is generated by encoder
                    0xfe=>extra.comments.push(body.to_vec()),
                    0xed if body.starts_with(b"Photoshop 3.0\0")=>{
                        // Keep standard IPTC resources. Photoshop thumbnails,
                        // resolution and other application-specific payloads
                        // are not blindly copied onto a different raster.
                        let mut i=14;let mut kept=b"Photoshop 3.0\0".to_vec();
                        while i<body.len() {
                            let begin=i;
                            if body.get(i..i+4)!=Some(b"8BIM"){extra.refuse("unknown Photoshop resource");break;}
                            let id=u16::from_be_bytes(body.get(i+4..i+6).ok_or("Truncated Photoshop resource")?.try_into().unwrap());i+=6;
                            let n=usize::from(*body.get(i).ok_or("Truncated Photoshop resource name")?);i+=(n+2)&!1;
                            let n=be32(body.get(i..i+4).ok_or("Truncated Photoshop resource length")?) as usize;i+=4;
                            i=i.checked_add(n+(n%2)).ok_or("Invalid Photoshop resource size")?;
                            if i>body.len(){return Err("Truncated Photoshop resource payload".into());}
                            match id {0x0404=>kept.extend_from_slice(&body[begin..i]),0x0409|0x040c=>{},_=>extra.refuse("Photoshop resources other than IPTC and generated thumbnails")}
                        }
                        if kept.len()>14 {extra.records.push(jpeg_segment(0xed,&kept)?);}
                    },
                    0xe0..=0xef=>{let _=start;extra.refuse("unrecognized JPEG application metadata");},
                    _=>{},
                }
                pos += length;
            }
        }
        Format::Png => {
            let mut pos = 8;
            while pos + 12 <= bytes.len() {
                let length = be32(&bytes[pos..pos+4]) as usize;
                let end = pos.checked_add(12).and_then(|v|v.checked_add(length)).ok_or("Invalid PNG chunk length")?;
                if end > bytes.len() { return Err("Truncated PNG chunk".into()); }
                let kind=&bytes[pos+4..pos+8];let body=&bytes[pos+8..end-4];
                match kind {
                    b"acTL" => return Err("Animated PNG editing is not supported".into()),
                    b"tEXt" | b"zTXt" | b"iTXt" => {
                        let (key,value)=png_text(kind,body)?;
                        if key==XMP_PNG {extra.set_xmp(&value)?;}
                        else if key==b"Comment" && kind!=b"iTXt" {extra.comments.push(value);}
                        else {
                            let property=match key { b"Title"=>Some("title"), b"Author"=>Some("creator"), b"Description"=>Some("description"), _=>None };
                            // International text may carry a locale and a translated keyword.
                            // Preserve those packets verbatim in PNG; do not flatten localized
                            // text into an unqualified Dublin Core value when converting.
                            let plain=kind!=b"iTXt" || body.get(key.len()+3..key.len()+5)==Some(&[0,0][..]);
                            if let Some(property)=property.filter(|_|plain) {
                                let value=if kind==b"iTXt" {String::from_utf8(value).map_err(|_|"PNG international text is not UTF-8")?}else{value.into_iter().map(char::from).collect()};
                                extra.descriptive.push(PngDescription{property,value,raw:bytes[pos..end].to_vec()});
                            }else{extra.records.push(bytes[pos..end].to_vec());}
                        }
                    },
                    b"pHYs" if body.len()==9=>{let (x,y)=(be32(&body[..4]),be32(&body[4..8]));if body[8]==1||x!=y{extra.density=Some((x,y,body[8]==1));}},
                    b"tIME" => extra.records.push(bytes[pos..end].to_vec()),
                    b"cHRM" | b"gAMA" | b"sRGB" => extra.color.push(bytes[pos..end].to_vec()),
                    // These describe the source palette/sample layout. The
                    // encoder writes full-precision truecolor samples and no
                    // indexed palette, so the old hints are not applicable.
                    b"sBIT" | b"hIST" | b"sPLT" => {},
                    b"bKGD"=>extra.refuse("PNG suggested background (source color model)"),
                    b"eXIf"=>{if extra.exif.is_some(){extra.refuse("multiple EXIF chunks");}else{extra.exif=Some(body.to_vec());}},
                    b"IHDR" | b"IDAT" | b"IEND" | b"PLTE" | b"tRNS" | b"iCCP" => {},
                    _ if kind[0].is_ascii_lowercase() && kind[3].is_ascii_lowercase()=>extra.records.push(bytes[pos..end].to_vec()),
                    _ => extra.refuse("unknown PNG metadata that is not safe to copy"),
                }
                pos = end;
            }
        }
        Format::Webp => {
            let mut pos = 12;
            while pos + 8 <= bytes.len() {
                let n = le32(&bytes[pos+4..pos+8]) as usize;
                let end = pos.checked_add(8).and_then(|v|v.checked_add(n)).and_then(|v|v.checked_add(n%2)).ok_or("Invalid WebP chunk length")?;
                if end > bytes.len() { return Err("Truncated WebP chunk".into()); }
                let body=&bytes[pos+8..pos+8+n];
                match &bytes[pos..pos+4] {
                    b"ANIM" | b"ANMF" => return Err("Animated WebP editing is not supported".into()),
                    b"VP8 " | b"VP8L" | b"VP8X" | b"ALPH" | b"ICCP" => {},
                    b"EXIF"=>{if extra.exif.is_some(){extra.refuse("multiple WebP EXIF chunks");}else{extra.exif=Some(body.strip_prefix(b"Exif\0\0").unwrap_or(body).to_vec());}},
                    b"XMP "=>extra.set_xmp(body)?,
                    _=>extra.refuse("unknown WebP metadata chunk"),
                }
                pos = end;
            }
        }
        Format::Gif => {
            if bytes.len() < 13 { return Err("Truncated GIF header".into()); }
            let mut pos = 13 + if bytes[10] & 0x80 != 0 { 3 * (2usize << (bytes[10] & 7)) } else { 0 };
            let mut images = 0;
            while let Some(&kind) = bytes.get(pos) {
                pos += 1;
                let mut comment=false;let mut data=Vec::new();
                match kind {
                    0x3b => break,
                    0x21 => {
                        let label = *bytes.get(pos).ok_or("Truncated GIF extension")?;pos += 1;
                        comment=label==0xfe;
                        if label!=0xf9&&!comment {extra.refuse("GIF application or plain-text extension");}
                    }
                    0x2c => {
                        images += 1;if images > 1 { return Err("Animated GIF editing is not supported".into()); }
                        let flags = *bytes.get(pos+8).ok_or("Truncated GIF image descriptor")?;
                        pos += 9 + if flags & 0x80 != 0 {3 * (2usize << (flags & 7))} else {0};pos += 1;
                    }
                    _ => return Err("Invalid GIF block".into()),
                }
                loop {
                    let n = usize::from(*bytes.get(pos).ok_or("Truncated GIF sub-block")?);pos += 1;
                    if n == 0 { break; }
                    if comment {data.extend_from_slice(bytes.get(pos..pos+n).ok_or("Truncated GIF comment")?);}
                    pos += n;if pos > bytes.len() { return Err("Truncated GIF sub-block".into()); }
                }
                if comment {extra.comments.push(data);}
            }
        }
        Format::Bmp => {
            let dib=le32(bytes.get(14..18).ok_or("Truncated BMP DIB header")?) as usize;
            if dib>=40 {
                let header=bytes.get(14..14+dib).ok_or("Truncated BMP DIB header")?;
                let x=le32(&header[24..28]);let y=le32(&header[28..32]);
                if x>0&&y>0&&x<=i32::MAX as u32&&y<=i32::MAX as u32 {extra.density=Some((x,y,true));}
                if dib>=108 {
                    match le32(&header[56..60]) {
                        0x73524742|0x57696e20=>{}, // sRGB / Windows color space
                        0x4d424544 if dib>=124=>{
                            let offset=14usize.checked_add(le32(&header[112..116]) as usize).ok_or("Invalid BMP ICC offset")?;
                            let size=le32(&header[116..120]) as usize;
                            let profile=bytes.get(offset..offset.checked_add(size).ok_or("Invalid BMP ICC size")?).ok_or("Truncated BMP ICC profile")?;
                            if size==0||offset<14+dib {return Err("Invalid BMP ICC profile location".into());}
                            extra.icc=Some(profile.to_vec());
                        },
                        _=>extra.refuse("BMP calibrated or linked color profile"),
                    }
                }
            } else if dib!=12 {extra.refuse("unrecognized BMP DIB metadata");}
        }
        Format::Same => unreachable!(),
    }
    Ok(extra)
}
fn jpeg_segment(marker:u8,data:&[u8])->Result<Vec<u8>,String> {
    let len=u16::try_from(data.len()+2).map_err(|_|"Metadata exceeds JPEG segment capacity")?;
    let mut out=vec![0xff,marker];out.extend(len.to_be_bytes());out.extend(data);Ok(out)
}
fn insert_jpeg_metadata(out:&mut Vec<u8>,data:Vec<u8>) {
    let at=if out.len()>=6&&out[2..4]==[0xff,0xe0] {let n=usize::from(u16::from_be_bytes([out[4],out[5]]));(4+n).min(out.len())}else{2};
    out.splice(at..at,data);
}
fn png_chunk(kind:&[u8;4],data:&[u8])->Vec<u8> {
    let mut out=Vec::new();out.extend((data.len() as u32).to_be_bytes());out.extend(kind);out.extend(data);
    let crc=crc32fast::hash(&out[4..]);out.extend(crc.to_be_bytes());out
}
fn webp_metadata(out:&mut Vec<u8>,kind:&[u8;4],data:&[u8],dims:(u32,u32))->Result<(),String> {
    if out.len()<20||&out[..4]!=b"RIFF"||&out[8..12]!=b"WEBP" {return Err("Invalid encoded WebP container".into());}
    let flag=if kind==b"EXIF" {0x08}else{0x04};
    if &out[12..16]==b"VP8X" {
        if out.len()<30||le32(&out[16..20])!=10{return Err("Invalid WebP extended header".into());}
        out[20]|=flag;
    } else {
        // Lossless bitstream header: bit 28 records whether alpha is used.
        if &out[12..16]!=b"VP8L"||out.get(20)!=Some(&0x2f)||out.len()<25{return Err("Unexpected encoded lossless WebP header".into());}
        let alpha=if le32(&out[21..25])&(1<<28)!=0 {0x10}else{0};
        let mut header=b"VP8X".to_vec();header.extend(10u32.to_le_bytes());header.extend([flag|alpha,0,0,0]);
        header.extend(&(dims.0-1).to_le_bytes()[..3]);header.extend(&(dims.1-1).to_le_bytes()[..3]);
        out.splice(12..12,header);
    }
    out.extend(kind);out.extend(u32::try_from(data.len()).map_err(err)?.to_le_bytes());out.extend(data);if data.len()%2!=0 {out.push(0);}
    let len=u32::try_from(out.len()-8).map_err(err)?;out[4..8].copy_from_slice(&len.to_le_bytes());Ok(())
}
fn bmp_icc(out:&mut Vec<u8>,icc:&[u8])->Result<(),String> {
    if out.len()<54||le32(&out[14..18])!=40 {return Err("Unexpected BMP encoder DIB header for ICC preservation".into());}
    let pixels=le32(&out[10..14]);out.splice(54..54,[0u8;84]);out[14..18].copy_from_slice(&124u32.to_le_bytes());
    out[10..14].copy_from_slice(&(pixels+84).to_le_bytes());out[70..74].copy_from_slice(&0x4d424544u32.to_le_bytes());
    out[122..126].copy_from_slice(&4u32.to_le_bytes());let offset=u32::try_from(out.len()-14).map_err(err)?;
    out[126..130].copy_from_slice(&offset.to_le_bytes());out[130..134].copy_from_slice(&u32::try_from(icc.len()).map_err(err)?.to_le_bytes());
    out.extend(icc);let len=u32::try_from(out.len()).map_err(err)?;out[2..6].copy_from_slice(&len.to_le_bytes());Ok(())
}
#[derive(Clone, Debug)]
pub(super) struct Decoded { pub pixels: Arc<DynamicImage>, pub format:Format, pub bytes:usize, pub digest:[u8;32], icc:Option<Vec<u8>>, exif:Option<Vec<u8>>, extra:ExtraMetadata }
pub(super) fn decode(source:&Source)->Result<Decoded,String> {
    let bytes=source.bytes()?;
    let format=image::guess_format(&bytes).ok().and_then(Format::from_image).ok_or("Unsupported image format")?;
    let mut extra=container_metadata(&bytes,format)?;
    let mut reader=ImageReader::with_format(Cursor::new(&bytes),format.image());
    let mut limits=Limits::default(); limits.max_alloc=Some(512*1024*1024); limits.max_image_width=Some(MAX_DIM); limits.max_image_height=Some(MAX_DIM); reader.limits(limits);
    let mut decoder=reader.into_decoder().map_err(err)?;
    admit(decoder.dimensions(),u64::from(decoder.color_type().bytes_per_pixel()))?;
    let container_icc=extra.icc.take();let container_exif=extra.exif.take();
    let icc=decoder.icc_profile().map_err(err)?.or(container_icc);
    let exif=decoder.exif_metadata().map_err(err)?.or(container_exif);
    let orientation=decoder.orientation().map_err(err)?;
    let orientation=if orientation==image::metadata::Orientation::NoTransforms {
        exif.as_deref().and_then(exif_orientation).unwrap_or(orientation)
    } else {orientation};
    let mut pixels=DynamicImage::from_decoder(decoder).map_err(err)?;
    pixels.apply_orientation(orientation);
    Ok(Decoded { pixels:Arc::new(pixels),format,bytes:bytes.len(),digest:fingerprint(&bytes),icc,exif,extra })
}
#[derive(Clone, Debug)]
pub(super) struct Encoded { pub geometry:Geometry, pub format:Format, pub bytes:Arc<Vec<u8>> }
fn transfer(v:f32)->f32 { if v<=0.04045 { v/12.92 } else { ((v+0.055)/1.055).powf(2.4) } }
fn inverse(v:f32)->f32 { if v<=0.0031308 { v*12.92 } else { 1.055*v.powf(1./2.4)-0.055 } }
trait Sample: image::Primitive + 'static {
    const LEVELS:usize;
    fn index(self)->usize;
    fn unit(self)->f32 { self.index() as f32/(Self::LEVELS-1) as f32 }
    fn quantize(v:f32)->Self;
}
impl Sample for u8 {
    const LEVELS:usize=256;
    fn index(self)->usize { usize::from(self) }
    fn quantize(v:f32)->Self { (v.clamp(0.,1.)*255.).round() as u8 }
}
impl Sample for u16 {
    const LEVELS:usize=65536;
    fn index(self)->usize { usize::from(self) }
    fn quantize(v:f32)->Self { (v.clamp(0.,1.)*65535.).round() as u16 }
}
type Raster<T> = image::ImageBuffer<Rgba<T>,Vec<T>>;

// Conventional separable Lanczos3/Catmull-Rom kernels, with antialiasing
// support widened for reductions. Weights are computed once per output
// coordinate, in source-pixel-center coordinates, and normalized at edges.
struct Weights { start:u32, values:Vec<f32> }
fn axis_weights(source:u32,target:u32,filter:usize,check:&impl Fn()->Result<(),String>)->Result<Vec<Weights>,String> {
    let scale=f64::from(source)/f64::from(target);
    let stretch=scale.max(1.);
    let support=if filter==1 {2.} else {3.};
    let sinc=|x:f64| if x==0. {1.} else {let a=std::f64::consts::PI*x;a.sin()/a};
    (0..target).map(|dst| {
        if dst%64==0 {check()?;}
        let center=(f64::from(dst)+0.5)*scale-0.5;
        let start=(center-support*stretch).ceil().max(0.) as u32;
        let end=((center+support*stretch).floor() as u32).min(source-1);
        let mut values:Vec<f32>=(start..=end).map(|src|{
            let x=((f64::from(src)-center)/stretch).abs();
            let v=if filter==1 { if x<=1. {1.5*x*x*x-2.5*x*x+1.}else if x<2. {-0.5*x*x*x+2.5*x*x-4.*x+2.}else{0.} }
                else if x<3. {sinc(x)*sinc(x/3.)}else{0.};
            v as f32
        }).collect();
        let sum: f32=values.iter().sum();
        for w in &mut values { *w/=sum; }
        Ok(Weights{start,values})
    }).collect()
}
fn weighted(w:&Weights,mut sample:impl FnMut(u32)->[f32;4])->[f32;4] {
    let mut out=[0.;4];
    for (i,weight) in w.values.iter().enumerate() {
        let p=sample(w.start+i as u32);
        for c in 0..4 {out[c]+=p[c]*weight;}
    }
    out
}
fn resize<T:Sample>(p:&Raster<T>,size:(u32,u32),s:&Settings,check:&impl Fn()->Result<(),String>)->Result<Raster<T>,String> where Rgba<T>:image::Pixel<Subpixel=T> {
    check()?;
    if p.dimensions()==size {return Ok(p.clone());}
    let (sw,sh)=p.dimensions();let (tw,th)=size;
    if s.filter==2 { // Nearest preserves original samples, including hidden RGB.
        return raster_rows(tw,th,check,|x,y|*p.get_pixel(
            ((u64::from(x)*2+1)*u64::from(sw)/(u64::from(tw)*2)) as u32,
            ((u64::from(y)*2+1)*u64::from(sh)/(u64::from(th)*2)) as u32));
    }
    let lut:Vec<f32>=(0..T::LEVELS).map(|i|{let v=i as f32/(T::LEVELS-1) as f32;if s.linear {transfer(v)} else {v}}).collect();
    let source=|x,y| {let p=p.get_pixel(x,y);let a=p[3].unit();[lut[p[0].index()]*a,lut[p[1].index()]*a,lut[p[2].index()]*a,a]};
    let output=|p:[f32;4]|{let a=p[3].clamp(0.,1.);let rgb=|i:usize|{let v=if a==0.{0.}else{(p[i]/a).clamp(0.,1.)};T::quantize(if s.linear{inverse(v)}else{v})};Rgba([rgb(0),rgb(1),rgb(2),T::quantize(a)])};
    let wx=axis_weights(sw,tw,s.filter,check)?;let wy=axis_weights(sh,th,s.filter,check)?;
    // Choose the pass order with the smaller memory footprint. The first
    // intermediate is already reduced along one axis for high-res scans.
    if area((tw,sh))<=area((sw,th)) {
        let mut mid=Vec::with_capacity(area((tw,sh)) as usize);
        for y in 0..sh {check()?;for x in 0..tw {mid.push(weighted(&wx[x as usize],|i|source(i,y)));}}
        raster_rows(tw,th,check,|x,y|output(weighted(&wy[y as usize],|i|mid[(u64::from(i)*u64::from(tw)+u64::from(x)) as usize])))
    } else {
        let mut mid=Vec::with_capacity(area((sw,th)) as usize);
        for y in 0..th {check()?;for x in 0..sw {mid.push(weighted(&wy[y as usize],|i|source(x,i)));}}
        raster_rows(tw,th,check,|x,y|output(weighted(&wx[x as usize],|i|mid[(u64::from(y)*u64::from(sw)+u64::from(i)) as usize])))
    }
}
// One cheap cancellation check per row. The contiguous output buffer and
// kernel evaluation order remain unchanged; no extra full-image copy.
fn raster_rows<T:Sample>(w:u32,h:u32,check:&impl Fn()->Result<(),String>,mut pixel:impl FnMut(u32,u32)->Rgba<T>)->Result<Raster<T>,String> where Rgba<T>:image::Pixel<Subpixel=T> {
    let mut raster=Raster::new(w,h);
    for (x,y,p) in raster.enumerate_pixels_mut(){if x==0{check()?;}*p=pixel(x,y);}
    Ok(raster)
}
fn center<T:Sample>(p:Raster<T>,size:(u32,u32),fill:Fill,pad:Rgba<T>)->Raster<T> where Rgba<T>:image::Pixel<Subpixel=T> {
    if p.dimensions()==size { return p; }
    if fill==Fill::Crop { imageops::crop_imm(&p,(p.width()-size.0)/2,(p.height()-size.1)/2,size.0,size.1).to_image() }
    else { let mut canvas=Raster::from_pixel(size.0,size.1,pad); imageops::replace(&mut canvas,&p,i64::from((size.0-p.width())/2),i64::from((size.1-p.height())/2)); canvas }
}
fn transform<T:Sample>(mut pixels:Raster<T>,plan:Geometry,s:&Settings,pad:Rgba<T>,check:&impl Fn()->Result<(),String>)->Result<Raster<T>,String> where Rgba<T>:image::Pixel<Subpixel=T> {
    check()?;
    pixels=match s.rotation {1=>imageops::rotate90(&pixels),2=>imageops::rotate180(&pixels),3=>imageops::rotate270(&pixels),_=>pixels};
    check()?;
    if s.flip_h {imageops::flip_horizontal_in_place(&mut pixels);check()?;}
    if s.flip_v {imageops::flip_vertical_in_place(&mut pixels);check()?;}
    if s.square!=Square::Off {pixels=center(pixels,plan.transformed,if s.square==Square::Crop{Fill::Crop}else{Fill::Pad},pad);}
    check()?;
    if !plan.kept&&s.mode!=Mode::Off&&(pixels.dimensions()!=plan.resample || pixels.dimensions()!=plan.target) {
        pixels=if pixels.dimensions()==plan.resample {pixels}else{resize(&pixels,plan.resample,s,check)?};
        check()?;
        pixels=center(pixels,plan.target,plan.fill,pad);
    }
    check()?;
    Ok(pixels)
}
fn flatten(p:&RgbaImage,pad:Rgba<u8>)->image::RgbImage { image::RgbImage::from_fn(p.width(),p.height(),|x,y| { let p=p.get_pixel(x,y); let a=u32::from(p[3]); image::Rgb(std::array::from_fn(|i| ((u32::from(p[i])*a+u32::from(pad[i])*(255-a)+127)/255) as u8)) }) }
/// Metadata support uses the pinned image encoders. Unsupported preservation
/// is refused rather than silently discarding an ICC profile or EXIF block.
pub(super) fn encode(input:&Decoded,s:&Settings)->Result<Encoded,String> {encode_checked(input,s,&||Ok(()))}
pub(super) fn encode_checked(input:&Decoded,s:&Settings,check:&impl Fn()->Result<(),String>)->Result<Encoded,String> {
    check()?;
    let plan=geometry(s,(input.pixels.width(),input.pixels.height()));
    let format=s.resolved_format(input.format);
    let high_precision=format==Format::Png && matches!(input.pixels.as_ref(),DynamicImage::ImageLuma16(_)|DynamicImage::ImageLumaA16(_)|DynamicImage::ImageRgb16(_)|DynamicImage::ImageRgba16(_));
    if s.metadata==Metadata::All {validate_metadata(input,format)?;}
    let pad=s.pad_color(format);
    let icc=if s.metadata!=Metadata::Strip { input.icc.clone() } else {None};
    let gray=match icc.as_deref(){
        None=>false,
        Some(profile) if profile.get(36..40)==Some(b"acsp".as_slice())=>match profile.get(16..20){Some(b"GRAY")=>true,Some(b"RGB ")=>false,_=>return Err("ICC color space is incompatible with the output encoder; choose Strip all".into())},
        Some(_)=>return Err("Invalid ICC profile header; choose Strip all".into()),
    };
    if gray&&!matches!(format,Format::Png|Format::Jpeg){return Err("Output format requires RGB samples; cannot keep a grayscale ICC profile. Choose PNG, JPEG or Strip all".into());}
    admit_work(input,plan,if high_precision {8}else{4},s,gray)?;
    let mut out=Vec::new();
    let (w,h)=plan.target;
    if high_precision {
        let pad16=Rgba(pad.0.map(|v|u16::from(v)*257));
        let pixels=transform(input.pixels.to_rgba16(),plan,s,pad16,check)?;
        let pixels=if gray {DynamicImage::ImageLumaA16(gray_alpha(&pixels,check)?)}else{DynamicImage::ImageRgba16(pixels)};
        check()?;
        let mut enc=image::codecs::png::PngEncoder::new(&mut out);
        if let Some(icc)=icc {enc.set_icc_profile(icc).map_err(err)?;}
        enc.write_image(pixels.as_bytes(),w,h,pixels.color().into()).map_err(err)?;
    } else {
        let pixels=transform(input.pixels.to_rgba8(),plan,s,pad,check)?;
        match format {
            Format::Jpeg=>{
                let mut enc=image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out,s.quality);
                if let Some(icc)=icc {enc.set_icc_profile(icc).map_err(err)?;}
                check()?;
                if gray {
                    let mut luma=Vec::with_capacity(area((w,h)) as usize);
                    for (x,_,p) in pixels.enumerate_pixels(){if x==0{check()?;}let a=u32::from(p[3]);let rgb:[u8;3]=std::array::from_fn(|i|((u32::from(p[i])*a+u32::from(pad[i])*(255-a)+127)/255) as u8);if rgb[0]!=rgb[1]||rgb[0]!=rgb[2]{return Err(gray_incompatible());}luma.push(rgb[0]);}
                    check()?;enc.write_image(&luma,w,h,image::ExtendedColorType::L8).map_err(err)?;
                }else{let rgb=flatten(&pixels,pad);check()?;enc.write_image(rgb.as_raw(),w,h,image::ExtendedColorType::Rgb8).map_err(err)?;}
            },
            Format::Png=>{
                let pixels=if gray {DynamicImage::ImageLumaA8(gray_alpha(&pixels,check)?)}else{DynamicImage::ImageRgba8(pixels)};
                check()?;let mut enc=image::codecs::png::PngEncoder::new(&mut out);
                if let Some(icc)=icc {enc.set_icc_profile(icc).map_err(err)?;}
                enc.write_image(pixels.as_bytes(),w,h,pixels.color().into()).map_err(err)?;
            },
            Format::Webp=>{check()?;let mut enc=image::codecs::webp::WebPEncoder::new_lossless(&mut out);if let Some(icc)=icc {enc.set_icc_profile(icc).map_err(err)?;}enc.write_image(pixels.as_raw(),w,h,image::ExtendedColorType::Rgba8).map_err(err)?;},
            Format::Gif|Format::Bmp=>{if format==Format::Gif&&icc.is_some(){return Err("GIF encoder cannot keep ICC; choose Strip all".into());}let p=if format.alpha(){DynamicImage::ImageRgba8(pixels)}else{DynamicImage::ImageRgb8(flatten(&pixels,pad))};check()?;p.write_to(&mut Cursor::new(&mut out),format.image()).map_err(err)?;if let Some(icc)=icc {bmp_icc(&mut out,&icc)?;}},
            Format::Same=>unreachable!(),
        }
    }
    check()?;
    if s.metadata==Metadata::All {write_metadata(&mut out,input,format,plan.target,s.rotation)?;}
    check()?;
    Ok(Encoded { geometry:plan,format,bytes:Arc::new(out) })
}
fn gray_incompatible()->String {"Colored padding or RGB samples cannot keep a grayscale ICC profile; choose neutral padding or Strip all".into()}
fn gray_alpha<T:Sample>(pixels:&Raster<T>,check:&impl Fn()->Result<(),String>)->Result<image::ImageBuffer<image::LumaA<T>,Vec<T>>,String> where Rgba<T>:image::Pixel<Subpixel=T> {
    let mut gray=image::ImageBuffer::new(pixels.width(),pixels.height());
    for (x,y,p) in pixels.enumerate_pixels(){if x==0{check()?;}if p[0]!=p[1]||p[0]!=p[2]{return Err(gray_incompatible());}gray.put_pixel(x,y,image::LumaA([p[0],p[3]]));}
    Ok(gray)
}
// XMP is RDF/XML. Keep unrelated bytes verbatim and edit only the standard
// raster properties, resolving prefixes in their lexical namespace scope.
// This is a restricted packet editor, not an XML/RDF converter: no DTDs,
// external entities, extended packets or ambiguous descriptive merges.
const RDF_NS:&str="http://www.w3.org/1999/02/22-rdf-syntax-ns#";
const DC_NS:&str="http://purl.org/dc/elements/1.1/";
const TIFF_NS:&str="http://ns.adobe.com/tiff/1.0/";
const EXIF_NS:&str="http://ns.adobe.com/exif/1.0/";
const XMP_NS:&str="http://ns.adobe.com/xap/1.0/";
const PS_NS:&str="http://ns.adobe.com/photoshop/1.0/";
#[derive(Clone,Copy)]
enum XmpRaster { Set(u32), Remove }
fn xmp_raster(ns:&str,name:&str,dims:(u32,u32))->Option<XmpRaster>{
    use XmpRaster::*;
    match (ns,name){
        (TIFF_NS,"Orientation")|(EXIF_NS,"Orientation")=>Some(Set(1)),
        (TIFF_NS,"ImageWidth")|(EXIF_NS,"PixelXDimension")|(EXIF_NS,"ImageWidth")=>Some(Set(dims.0)),
        (TIFF_NS,"ImageLength")|(TIFF_NS,"ImageHeight")|(EXIF_NS,"PixelYDimension")|(EXIF_NS,"ImageHeight")=>Some(Set(dims.1)),
        (XMP_NS,"Thumbnails")|(PS_NS,"ImageSize")=>Some(Remove),
        _=>None,
    }
}
fn xml_text(value:&str)->Result<String,String>{
    let mut out=String::new();
    for c in value.chars(){match c{'&'=>out.push_str("&amp;"),'<'=>out.push_str("&lt;"),'>'=>out.push_str("&gt;"),'\r'=>out.push_str("&#13;"),'\t'|'\n'=>out.push(c),c if c>='\u{20}'&&c!='\u{fffe}'&&c!='\u{ffff}'=>out.push(c),_=>return Err("PNG descriptive text contains characters that cannot be represented in XML".into())}}
    Ok(out)
}
fn xmp_description(fields:&[PngDescription])->Result<String,String>{
    if fields.is_empty(){return Ok(String::new());}
    let mut out=format!(r#"<rdf:Description xmlns:rdf="{RDF_NS}" xmlns:dc="{DC_NS}" rdf:about="">"#);
    for (i,f) in fields.iter().enumerate(){
        if fields[..i].iter().any(|old|old.property==f.property){return Err("Multiple PNG descriptive fields cannot be merged into a single XMP property".into());}
        let value=xml_text(&f.value)?;
        match f.property {
            "creator"=>out.push_str(&format!("<dc:creator><rdf:Seq><rdf:li>{value}</rdf:li></rdf:Seq></dc:creator>")),
            property=>out.push_str(&format!(r#"<dc:{property}><rdf:Alt><rdf:li xml:lang="x-default">{value}</rdf:li></rdf:Alt></dc:{property}>"#)),
        }
    }
    out.push_str("</rdf:Description>");Ok(out)
}
fn updated_xmp(packet:Option<&[u8]>,fields:&[PngDescription],dims:(u32,u32))->Result<Option<Vec<u8>>,String>{
    use std::{collections::HashMap,ops::Range,sync::OnceLock};
    use regex::Regex;
    let addition=xmp_description(fields)?;
    let Some(packet)=packet else {
        return Ok(if addition.is_empty(){None}else{Some(format!(r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="{RDF_NS}">{addition}</rdf:RDF></x:xmpmeta>"#).into_bytes())});
    };
    let xml=std::str::from_utf8(packet).map_err(|_|"XMP packet is not UTF-8")?;
    // Tokenize only XML markup and text. A literal '<' in a quoted value,
    // comment or CDATA does not start an element. Reject unsupported markup
    // instead of applying substitutions to an arbitrary byte sequence.
    static TOKENS:OnceLock<Regex>=OnceLock::new();
    static NAME:OnceLock<Regex>=OnceLock::new();
    static ATTR:OnceLock<Regex>=OnceLock::new();
    let tokens=TOKENS.get_or_init(||Regex::new(r#"(?s)<!--.*?-->|<\?.*?\?>|<!\[CDATA\[.*?\]\]>|</[^\s<>/]+\s*>|<[^\s<>/!?=]+(?:[^<>"']|"[^"]*"|'[^']*')*>|[^<]+"#).unwrap());
    let name=NAME.get_or_init(||Regex::new(r"^<([^\s<>/=]+)").unwrap());
    let attr=ATTR.get_or_init(||Regex::new(r#"(?s)^\s+([^\s<>=/]+)\s*=\s*("[^"]*"|'[^']*')"#).unwrap());
    type Scope<'a>=HashMap<&'a str,&'a str>;
    fn qualified<'a>(q:&'a str,scope:&Scope<'a>,attribute:bool)->(&'a str,&'a str){
        if let Some((prefix,local))=q.split_once(':'){(scope.get(prefix).copied().unwrap_or(""),local)}else{(if attribute{""}else{scope.get("").copied().unwrap_or("")},q)}
    }
    struct Element<'a>{q:&'a str,scope:Scope<'a>,start:usize,open_end:usize,target:Option<XmpRaster>,hidden:bool,simple:bool,rdf:bool}
    let mut stack:Vec<Element<'_>>=Vec::new();let mut edits:Vec<(Range<usize>,String)>=Vec::new();let mut end=0;let mut rdf_count=0;let mut roots=0;
    let invalid=||"Unsupported or malformed XMP XML packet".to_string();
    let duplicate=|ns:&str,local:&str|ns==DC_NS&&fields.iter().any(|f|f.property==local);
    for token in tokens.find_iter(xml){
        if token.start()!=end{return Err(invalid());}end=token.end();let text=token.as_str();
        if text.starts_with("</"){
            let q=text[2..text.len()-1].trim();let element=stack.pop().ok_or_else(invalid)?;if q!=element.q{return Err(invalid());}
            if !element.hidden {
                if let Some(target)=element.target {match target {
                    XmpRaster::Set(value) if element.simple=>edits.push((element.open_end..token.start(),value.to_string())),
                    _=>edits.push((element.start..end,String::new())),
                }}else if element.rdf&&!addition.is_empty(){edits.push((token.start()..token.start(),addition.clone()));}
            }
            continue;
        }
        if !text.starts_with('<'){if stack.is_empty()&&!text.trim_matches(|c:char|c.is_whitespace()||c=='\u{feff}').is_empty(){return Err(invalid());}continue;}
        if text.starts_with("<!--")||text.starts_with("<?")||text.starts_with("<![CDATA["){
            if let Some(parent)=stack.last_mut(){parent.simple=false;}continue;
        }
        if stack.len()>=128{return Err("XMP nesting exceeds supported packet depth".into());}
        let hidden=stack.last().is_some_and(|p|p.hidden||p.target.is_some());
        if let Some(parent)=stack.last_mut(){parent.simple=false;}else{roots+=1;}
        let q=name.captures(text).and_then(|c|c.get(1)).ok_or_else(invalid)?.as_str();
        let self_close=text.ends_with("/>");let attr_end=text.len()-if self_close{2}else{1};
        let mut pos=1+q.len();let mut attrs=Vec::new();let mut scope=stack.last().map(|p|p.scope.clone()).unwrap_or_default();
        while pos<attr_end{
            if text[pos..attr_end].trim().is_empty(){break;}
            let caps=attr.captures(&text[pos..attr_end]).ok_or_else(invalid)?;
            let whole=caps.get(0).unwrap();let key=caps.get(1).unwrap().as_str();let quoted=caps.get(2).unwrap();let value=&quoted.as_str()[1..quoted.as_str().len()-1];
            if key=="xmlns"||key.starts_with("xmlns:"){
                if value.contains('&') {return Err("Escaped XMP namespace URI is unsupported".into());}
                scope.insert(key.strip_prefix("xmlns:").unwrap_or(""),value);
                if scope.len()>128{return Err("XMP has too many namespace bindings".into());}
            }else{attrs.push((key,token.start()+pos..token.start()+pos+whole.end(),token.start()+pos+quoted.start()+1..token.start()+pos+quoted.end()-1));}
            pos+=whole.end();
        }
        let(ns,local)=qualified(q,&scope,false);let target=if hidden{None}else{xmp_raster(ns,local,dims)};
        let rdf=!hidden&&target.is_none()&&ns==RDF_NS&&local=="RDF";
        if rdf{rdf_count+=1;}
        if !hidden&&target.is_none(){
            if duplicate(ns,local){return Err("PNG descriptive field conflicts with an existing XMP property".into());}
            for (key,whole,value) in &attrs{
                let(ns,local)=qualified(key,&scope,true);
                if duplicate(ns,local){return Err("PNG descriptive field conflicts with an existing XMP property".into());}
                match xmp_raster(ns,local,dims){Some(XmpRaster::Set(n))=>edits.push((value.clone(),n.to_string())),Some(XmpRaster::Remove)=>edits.push((whole.clone(),String::new())),None=>{}}
            }
        }
        if self_close {
            if target.is_some(){edits.push((token.start()..end,String::new()));}
            else if rdf&&!addition.is_empty(){edits.push((end-2..end,format!(">{addition}</{q}>")));}
        }else{stack.push(Element{q,scope,start:token.start(),open_end:end,target,hidden,simple:attrs.is_empty(),rdf});}
    }
    if end!=xml.len()||!stack.is_empty()||roots!=1||rdf_count!=1{return Err(invalid());}
    edits.sort_by_key(|(range,_)|(range.start,range.end));
    let mut out=String::with_capacity(xml.len()+addition.len());let mut pos=0;
    for(range,replacement)in edits{if range.start<pos{return Err(invalid());}out.push_str(&xml[pos..range.start]);out.push_str(&replacement);pos=range.end;}
    out.push_str(&xml[pos..]);Ok(Some(out.into_bytes()))
}

fn validate_metadata(input:&Decoded,format:Format)->Result<(),String> {
    let extra=&input.extra;
    let refuse=|what:&str|Err(format!("Cannot keep {what} in {} output; choose Keep ICC only or Strip all",format.short()));
    if let Some(what)=&extra.unsupported {return refuse(what);}
    if format!=input.format&&!extra.records.is_empty(){return refuse("container-specific descriptive metadata");}
    if input.exif.is_some()&&!matches!(format,Format::Jpeg|Format::Png|Format::Webp){return refuse("EXIF metadata");}
    if extra.xmp.is_some()&&!matches!(format,Format::Jpeg|Format::Png|Format::Webp){return refuse("XMP metadata");}
    if !extra.descriptive.is_empty()&&!matches!(format,Format::Jpeg|Format::Png|Format::Webp){return refuse("PNG descriptive text");}
    if !extra.comments.is_empty()&&!matches!(format,Format::Jpeg|Format::Png|Format::Gif){return refuse("comment metadata");}
    if format==Format::Png&&extra.comments.iter().any(|v|v.contains(&0)){return refuse("binary comments in PNG text");}
    if format!=Format::Png&&!extra.color.is_empty()&&input.icc.is_none() {
        // Standard sRGB signaling is implicit in the target encodings. A
        // nonstandard gamma/chromaticity cannot be accurately represented
        // without an ICC transform/profile; never relabel it as sRGB.
        let standard=extra.color.iter().all(|r|match &r[4..8] {
            b"sRGB"=>true,b"gAMA"=>r.len()==16&&be32(&r[8..12])==45455,
            b"cHRM"=>r.len()==44&&r[8..40].chunks_exact(4).map(be32).eq([31270,32900,64000,33000,30000,60000,15000,6000]),_=>false});
        if !standard{return refuse("non-sRGB PNG color information without ICC");}
    }
    if extra.density.is_some()&&format==Format::Gif{return refuse("pixel-density metadata");}
    if extra.density.is_some_and(|(_,_,physical)|!physical)&&format==Format::Bmp{return refuse("unitless pixel aspect metadata");}
    Ok(())
}
fn write_metadata(out:&mut Vec<u8>,input:&Decoded,format:Format,dims:(u32,u32),rotation:usize)->Result<(),String> {
    let extra=&input.extra;
    if input.exif.is_some()||(format==Format::Webp&&extra.density.is_some()) {let mut exif=input.exif.as_ref().map(|exif|exif.strip_prefix(b"Exif\0\0").unwrap_or(exif).to_vec()).unwrap_or_else(||b"II\x2a\0\x08\0\0\0\0\0\0\0\0\0".to_vec());normalize_exif(&mut exif,Some(dims))?;
        if format==Format::Webp {if let Some((mut x,mut y,physical))=extra.density {if rotation%2==1 {std::mem::swap(&mut x,&mut y);}set_exif_density(&mut exif,x,y,physical)?;}}
        match format {Format::Jpeg=>{let mut body=b"Exif\0\0".to_vec();body.extend(exif);insert_jpeg_metadata(out,jpeg_segment(0xe1,&body)?);},Format::Png=>{out.splice(33..33,png_chunk(b"eXIf",&exif));},Format::Webp=>webp_metadata(out,b"EXIF",&exif,dims)?,_=>unreachable!()}
    }
    let fields=if format==Format::Png {&[][..]}else{extra.descriptive.as_slice()};
    if let Some(xmp)=updated_xmp(extra.xmp.as_deref(),fields,dims)? {match format {
        Format::Jpeg=>{let mut body=XMP_JPEG.to_vec();body.extend(&xmp);insert_jpeg_metadata(out,jpeg_segment(0xe1,&body)?);},
        Format::Png=>{let mut body=XMP_PNG.to_vec();body.extend([0,0,0,0,0]);body.extend(&xmp);out.splice(33..33,png_chunk(b"iTXt",&body));},
        Format::Webp=>webp_metadata(out,b"XMP ",&xmp,dims)?,_=>unreachable!()
    }}
    if format==Format::Png {for field in &extra.descriptive{out.splice(33..33,field.raw.iter().copied());}}
    for comment in &extra.comments {match format {
        Format::Jpeg=>{insert_jpeg_metadata(out,jpeg_segment(0xfe,comment)?);},
        Format::Png=>{let mut body=b"Comment\0".to_vec();body.extend(comment);out.splice(33..33,png_chunk(b"tEXt",&body));},
        Format::Gif=>{let mut body=vec![0x21,0xfe];for chunk in comment.chunks(255){body.push(chunk.len() as u8);body.extend(chunk);}body.push(0);let end=out.len().checked_sub(1).ok_or("Empty GIF output")?;if out[end]!=0x3b{return Err("Invalid GIF output trailer".into());}out.splice(end..end,body);},_=>unreachable!()
    }}
    if !extra.records.is_empty() {match format {Format::Jpeg=>insert_jpeg_metadata(out,extra.records.iter().flatten().copied().collect()),Format::Png=>{out.splice(33..33,extra.records.iter().flatten().copied());},_=>return Err("Output cannot preserve container-specific metadata".into())}}
    if format==Format::Png&&!extra.color.is_empty(){out.splice(33..33,extra.color.iter().flatten().copied());}
    if let Some((mut x,mut y,physical))=extra.density {
        if rotation%2==1 {std::mem::swap(&mut x,&mut y);}
        match format {
            Format::Png=>{let mut body=x.to_be_bytes().to_vec();body.extend(y.to_be_bytes());body.push(u8::from(physical));out.splice(33..33,png_chunk(b"pHYs",&body));},
            Format::Bmp=>{out[38..42].copy_from_slice(&x.to_le_bytes());out[42..46].copy_from_slice(&y.to_le_bytes());},
            Format::Jpeg=>{
                // Prefer dots/cm; use dpi when its conversion has less error.
                let (x,y,unit)=if physical {
                    let cm=(f64::from(x)/100.,f64::from(y)/100.);let inch=(f64::from(x)*0.0254,f64::from(y)*0.0254);
                    let error=|(a,b):(f64,f64)| (a-a.round()).abs()+(b-b.round()).abs();
                    if error(inch)<error(cm) {(inch.0.round() as u32,inch.1.round() as u32,1)}else{(cm.0.round() as u32,cm.1.round() as u32,2)}
                }else{(x,y,0)};
                let x=u16::try_from(x).map_err(|_|"Pixel density exceeds JPEG capacity")?;let y=u16::try_from(y).map_err(|_|"Pixel density exceeds JPEG capacity")?;
                if x==0||y==0{return Err("Pixel density is below JPEG capacity".into());}
                // The encoder's JFIF APP0 may follow inserted metadata. Find
                // the actual marker by walking the segment stream.
                let mut pos=2;let mut found=false;
                while pos+4<=out.len()&&out[pos]==0xff {let marker=out[pos+1];if marker==0xda||marker==0xd9{break;}let n=usize::from(u16::from_be_bytes([out[pos+2],out[pos+3]]));if n<2||pos+2+n>out.len(){break;}
                    if marker==0xe0&&n>=16&&out[pos+4..].starts_with(b"JFIF\0") {out[pos+11]=unit;out[pos+12..pos+14].copy_from_slice(&x.to_be_bytes());out[pos+14..pos+16].copy_from_slice(&y.to_be_bytes());found=true;break;}pos+=2+n;
                }
                if !found {let mut body=b"JFIF\0\x01\x02".to_vec();body.push(unit);body.extend(x.to_be_bytes());body.extend(y.to_be_bytes());body.extend([0,0]);out.splice(2..2,jpeg_segment(0xe0,&body)?);}
            },Format::Webp=>{},_=>unreachable!()
        }
    }
    Ok(())
}

// The image decoder's metadata API is the primary authority. This small
// TIFF-directory reader additionally handles EXIF in containers for which the
// pinned codec does not expose the orientation. No TIFF image decoder here.
fn exif_orientation(data:&[u8])->Option<image::metadata::Orientation> {
    use image::metadata::Orientation;
    let data=data.strip_prefix(b"Exif\0\0").unwrap_or(data);
    let little=match data.get(..4)? {b"II\x2a\0"=>true,b"MM\0\x2a"=>false,_=>return None};
    let u16_at=|offset:usize| {let b:[u8;2]=data.get(offset..offset.checked_add(2)?)?.try_into().ok()?;Some(if little{u16::from_le_bytes(b)}else{u16::from_be_bytes(b)})};
    let offset={let b:[u8;4]=data.get(4..8)?.try_into().ok()?;if little{u32::from_le_bytes(b)}else{u32::from_be_bytes(b)}} as usize;
    let n=usize::from(u16_at(offset)?);
    for i in 0..n {let entry=offset.checked_add(2+i*12)?;if u16_at(entry)==Some(0x0112){return Orientation::from_exif(u8::try_from(u16_at(entry+8)?).ok()?);}}
    None
}
#[cfg(test)]
fn normalize_exif_orientation(exif:&mut Vec<u8>)->Result<(),String>{normalize_exif(exif,None)}
fn normalize_exif(exif: &mut Vec<u8>, dims:Option<(u32,u32)>) -> Result<(), String> {
    let little = match exif.get(..4) {Some(b"II\x2a\0")=>true,Some(b"MM\0\x2a")=>false,_=>return Err("Invalid EXIF TIFF header".into())};
    let u16_at = |bytes: &[u8], offset: usize| -> Option<u16> {let a:[u8;2]=bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?;Some(if little{u16::from_le_bytes(a)}else{u16::from_be_bytes(a)})};
    let u32_at = |bytes: &[u8], offset: usize| -> Option<u32> {let a:[u8;4]=bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?;Some(if little{u32::from_le_bytes(a)}else{u32::from_be_bytes(a)})};
    let enc16=|v:u16|if little{v.to_le_bytes()}else{v.to_be_bytes()};let enc32=|v:u32|if little{v.to_le_bytes()}else{v.to_be_bytes()};
    let ifd0=u32_at(exif,4).ok_or("Truncated EXIF header")? as usize;
    let mut pointer=Some(ifd0);let mut seen=Vec::new();let mut have_orientation=false;
    while let Some(offset)=pointer.take() {
        if seen.contains(&offset){return Err("Cyclic EXIF directory".into());}seen.push(offset);
        let count=usize::from(u16_at(exif,offset).ok_or("Truncated EXIF directory")?);let start=offset.checked_add(2).ok_or("Invalid EXIF offset")?;let end=start.checked_add(count*12).ok_or("Invalid EXIF directory size")?;
        if end.checked_add(4).is_none_or(|n|n>exif.len()){return Err("Truncated EXIF directory".into());}
        for entry in (start..end).step_by(12) {
            let tag=u16_at(exif,entry).unwrap();let ty=u16_at(exif,entry+2).unwrap();let n=u32_at(exif,entry+4).unwrap();
            if tag==0x8769&&offset==ifd0 {if ty!=4||n!=1{return Err("Invalid EXIF subdirectory pointer".into());}pointer=Some(u32_at(exif,entry+8).unwrap() as usize);}
            let value=match tag {0x0112=>{if offset==ifd0{have_orientation=true;}Some(1)},0x0100|0xa002=>dims.map(|d|d.0),0x0101|0xa003=>dims.map(|d|d.1),_=>None};
            if let Some(value)=value {if n!=1||!matches!(ty,3|4){return Err("Unsupported EXIF orientation/dimension representation".into());}if ty==3{let v=u16::try_from(value).map_err(err)?;exif[entry+8..entry+10].copy_from_slice(&enc16(v));}else{exif[entry+8..entry+12].copy_from_slice(&enc32(value));}}
        }
        // IFD1 is the thumbnail directory. Unlink it: the thumbnail is
        // derived from the old raster and must not be advertised as current.
        // TIFF value offsets elsewhere stay unchanged, including maker notes.
        if dims.is_some()&&offset==ifd0 {exif[end..end+4].fill(0);}
    }
    if !have_orientation&&dims.is_some() {
        // Append a replacement IFD0 rather than relocating any value data.
        let count=usize::from(u16_at(exif,ifd0).unwrap());let start=ifd0+2;let end=start+12*count;
        let mut entries:Vec<[u8;12]>=exif[start..end].chunks_exact(12).map(|v|v.try_into().unwrap()).collect();
        let mut orientation=[0u8;12];orientation[..2].copy_from_slice(&enc16(0x0112));orientation[2..4].copy_from_slice(&enc16(3));orientation[4..8].copy_from_slice(&enc32(1));orientation[8..10].copy_from_slice(&enc16(1));entries.push(orientation);entries.sort_by_key(|v|u16_at(v,0).unwrap());
        if exif.len()%2!=0{exif.push(0);}let offset=u32::try_from(exif.len()).map_err(err)?;exif[4..8].copy_from_slice(&enc32(offset));
        exif.extend(enc16(u16::try_from(entries.len()).map_err(err)?));for entry in entries{exif.extend(entry);}exif.extend([0;4]);
    }
    Ok(())
}
// EXIF is WebP's standard carrier for physical density or pixel aspect.
// Append rational values and a new IFD0 without moving existing value data.
fn set_exif_density(exif:&mut Vec<u8>,x:u32,y:u32,physical:bool)->Result<(),String>{
    let little=match exif.get(..4){Some(b"II\x2a\0")=>true,Some(b"MM\0\x2a")=>false,_=>return Err("Invalid EXIF TIFF header".into())};
    let enc16=|v:u16|if little{v.to_le_bytes()}else{v.to_be_bytes()};let enc32=|v:u32|if little{v.to_le_bytes()}else{v.to_be_bytes()};
    let dec16=|b:&[u8]|if little{u16::from_le_bytes(b.try_into().unwrap())}else{u16::from_be_bytes(b.try_into().unwrap())};
    let dec32=|b:&[u8]|if little{u32::from_le_bytes(b.try_into().unwrap())}else{u32::from_be_bytes(b.try_into().unwrap())};
    let offset=dec32(exif.get(4..8).ok_or("Truncated EXIF header")?) as usize;
    let count=usize::from(dec16(exif.get(offset..offset.checked_add(2).ok_or("Invalid EXIF IFD")?).ok_or("Truncated EXIF IFD")?));
    let start=offset+2;let end=start.checked_add(count*12).ok_or("Invalid EXIF IFD")?;
    let mut entries:Vec<[u8;12]>=exif.get(start..end).ok_or("Truncated EXIF IFD")?.chunks_exact(12).filter(|entry|!matches!(dec16(&entry[..2]),0x011a|0x011b|0x0128)).map(|e|e.try_into().unwrap()).collect();
    if exif.len()%2!=0{exif.push(0);}
    for (tag,value) in [(0x011a,x),(0x011b,y)] {let ptr=u32::try_from(exif.len()).map_err(err)?;exif.extend(enc32(value));exif.extend(enc32(if physical{100}else{1}));let mut entry=[0u8;12];entry[..2].copy_from_slice(&enc16(tag));entry[2..4].copy_from_slice(&enc16(5));entry[4..8].copy_from_slice(&enc32(1));entry[8..12].copy_from_slice(&enc32(ptr));entries.push(entry);}
    let mut unit=[0u8;12];unit[..2].copy_from_slice(&enc16(0x0128));unit[2..4].copy_from_slice(&enc16(3));unit[4..8].copy_from_slice(&enc32(1));unit[8..10].copy_from_slice(&enc16(if physical{3}else{1}));entries.push(unit);entries.sort_by_key(|e|dec16(&e[..2]));
    let offset=u32::try_from(exif.len()).map_err(err)?;exif[4..8].copy_from_slice(&enc32(offset));exif.extend(enc16(u16::try_from(entries.len()).map_err(err)?));for entry in entries{exif.extend(entry);}exif.extend([0;4]);Ok(())
}
fn safe_component(s:&str)->String { s.chars().map(|c|if c=='/'||c=='\\'||c.is_control() { '_' } else {c}).collect() }
pub(super) fn output_path(source:&Source,encoded:&Encoded,template:&str)->Result<PathBuf,String> {
    let path=source.path(); let dir=path.parent().ok_or("Source has no parent directory")?;
    let name=path.file_stem().and_then(|p|p.to_str()).ok_or("Non-UTF-8 source filename cannot be expanded")?;
    let (album,artist,pic)=match source { Source::Picture{album,artist,kind,..}=>(safe_component(album),safe_component(artist),match kind {PictureType::CoverFront=>"front",PictureType::CoverBack=>"back",_=>"other"}), _=>(String::new(),String::new(),"") };
    let directory=dir.to_str().ok_or("Non-UTF-8 source directory cannot be expanded")?;
    let w=encoded.geometry.target.0.to_string(); let h=encoded.geometry.target.1.to_string();
    let fields=[("DIR",directory),("ALBUMDIR",directory),("NAME",name),("EXT",encoded.format.extension()),("W",&w),("H",&h),("ALBUM",&album),("ARTIST",&artist),("PICTYPE",pic)];
    // Expand in one pass. A filename/tag containing %W% is literal data.
    let mut result=String::new(); let mut rest=template;
    while let Some(i)=rest.find('%') { result.push_str(&rest[..i]); let tail=&rest[i+1..]; let end=tail.find('%').ok_or("Unclosed template token")?; let token=&tail[..end]; if !source.embedded() && matches!(token,"ALBUMDIR"|"ALBUM"|"ARTIST"|"PICTYPE") {return Err(format!("Token %{token}% requires embedded artwork"));} let value=fields.iter().find(|(k,_)|*k==token).ok_or_else(||format!("Unknown template token %{token}%"))?.1; result.push_str(value); rest=&tail[end+1..]; }
    result.push_str(rest);
    if result.contains('\0') {return Err("Output path contains a NUL character".into());}
    let result=if let Some(rest)=result.strip_prefix("~/") { let home=std::env::var_os("HOME").or_else(||std::env::var_os("USERPROFILE")).ok_or("Home directory is unavailable")?; PathBuf::from(home).join(rest) } else { PathBuf::from(result) };
    if result.file_name().is_none() || template.trim().is_empty() || template.ends_with('/') { return Err("Choose an output filename".into()); }
    Ok(if result.is_absolute() {result} else {dir.join(result)})
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Destination { Write(PathBuf,bool), Unchanged(PathBuf), Skip, Conflict(PathBuf) }
fn identical(path:&Path,bytes:&[u8])->Result<bool,String> {
    let md=fs::symlink_metadata(path).map_err(err)?;
    if !md.is_file() { return Err(format!("Destination is not a regular file: {}",path.display())); }
    if md.len()!=bytes.len() as u64 { return Ok(false); }
    let mut reader=fs::File::open(path).map_err(err)?; let mut buf=[0u8;65536];
    for part in bytes.chunks(buf.len()) { reader.read_exact(&mut buf[..part.len()]).map_err(err)?; if &buf[..part.len()]!=part { return Ok(false); } } Ok(true)
}
pub(super) fn destination(path:&Path,bytes:&[u8],policy:Conflict)->Result<Destination,String> {
    for n in 0..100_000u32 { let candidate=if n==0 {path.to_path_buf()} else { let mut name=path.file_stem().ok_or("Missing output filename")?.to_os_string(); name.push(format!("_{n}")); if let Some(ext)=path.extension() {name.push(".");name.push(ext);} path.with_file_name(name) };
        match fs::symlink_metadata(&candidate) {
            Err(e) if e.kind()==std::io::ErrorKind::NotFound=>return Ok(Destination::Write(candidate,false)), Err(e)=>return Err(err(e)), Ok(_)=>{},
        }
        if identical(&candidate,bytes)? {return Ok(Destination::Unchanged(candidate));}
        if n==0 { match policy { Conflict::Ask=>return Ok(Destination::Conflict(candidate)),Conflict::Replace=>return Ok(Destination::Write(candidate,true)),Conflict::Skip=>return Ok(Destination::Skip),Conflict::Number=>{} } }
    } Err("No available numbered filename".into())
}
/// Stage in the destination filesystem, publish atomically, and let the RAII
/// guard remove the temporary file on every ordinary error path. No sidecars.
#[cfg(test)]
fn write_image(path:&Path,bytes:&[u8],replace:bool)->Result<(),String> { write_image_checked(path,bytes,replace,&||Ok(())) }
#[cfg(test)]
fn write_image_checked(path:&Path,bytes:&[u8],replace:bool,check:&impl Fn()->Result<(),String>)->Result<(),String> {
    write_image_with_commit(path,bytes,replace,check,check).map(|_|())
}
pub(super) fn write_image_with_commit(path:&Path,bytes:&[u8],replace:bool,check:&impl Fn()->Result<(),String>,commit:&impl Fn()->Result<(),String>)->Result<bool,String> {
    check()?;
    if replace && identical(path,bytes)? {return Ok(false);}
    let parent=path.parent().ok_or("Missing output directory")?;
    // A nonexistent folder is an explicit blocker; avoid creating unintended
    // directory trees or leaving them behind after a failed write.
    let mut tmp=tempfile::NamedTempFile::new_in(parent).map_err(err)?;
    tmp.write_all(bytes).map_err(err)?; tmp.as_file().sync_all().map_err(err)?;
    commit()?;
    if replace { let md=fs::symlink_metadata(path).map_err(err)?; if !md.is_file() {return Err("Destination changed before replacement".into());} tmp.as_file().set_permissions(md.permissions()).map_err(err)?; tmp.persist(path).map_err(err)?; }
    else { tmp.persist_noclobber(path).map_err(err)?; }
    Ok(true)
}
/// Native FLAC and DSF writes use their existing journaled writers. Only
/// established Lofty-writable carriers use an isolated same-filesystem
/// copy, published after the tag and picture verification.
pub(super) fn reembed_checked(source:&Source,encoded:&Encoded,check:&impl Fn()->Result<(),String>,cancel:&super::probe::MetadataWriteCancelFlag)->Result<bool,String> {
    check()?;
    let Source::Picture{path,tag,index,kind,description,digest,..}=source else {return Err("No embedded picture selected".into());};
    let tag=tag.ok_or("This artwork carrier is readable but has no supported artwork writer; choose File")?;
    let (mime,mime_text)=match encoded.format {Format::Jpeg=>(MimeType::Jpeg,"image/jpeg"),Format::Png=>(MimeType::Png,"image/png"),_=>return Err("Embedded artwork must be JPEG or PNG".into())};
    if !fs::symlink_metadata(path).map_err(err)?.is_file() { return Err("Audio target must be a regular, non-symlink file".into()); }
    let stamp=fs::symlink_metadata(path).map_err(err)?;
    let original=lofty::read_from_path(path).map_err(err)?;
    let original_tag=original.tag(tag).ok_or("Picture tag disappeared")?;
    let selected=original_tag.pictures().get(*index).ok_or("Picture disappeared")?;
    if fingerprint(selected.data())!=*digest || selected.pic_type()!=*kind || selected.description()!=description.as_deref() { return Err("Picture changed; reopen Edit Image".into()); }
    if selected.data()==encoded.bytes.as_slice() { return Ok(false); }
    check()?;
    if super::probe::replace_editor_picture_native(path,tag,*index,selected,mime_text,encoded.bytes.as_slice(),cancel)? { return Ok(true); }
    let replacement=Picture::new_unchecked(selected.pic_type(),Some(mime),selected.description().map(str::to_owned),encoded.bytes.as_ref().clone());
    let mut expected=original_tag.pictures().to_vec(); expected[*index]=replacement.clone();
    let parent=path.parent().ok_or("Audio target has no parent")?;
    let suffix=path.extension().and_then(|v|v.to_str()).map(|v|format!(".{v}")).unwrap_or_default();
    let tmp=tempfile::Builder::new().suffix(&suffix).tempfile_in(parent).map_err(err)?;
    fs::copy(path,tmp.path()).map_err(err)?;
    check()?;
    let mut staged=lofty::read_from_path(tmp.path()).map_err(err)?;
    staged.tag_mut(tag).ok_or("Staged picture tag disappeared")?.set_picture(*index,replacement);
    staged.save_to_path(tmp.path(),lofty::config::WriteOptions::default()).map_err(err)?;
    let verify=lofty::read_from_path(tmp.path()).map_err(err)?;
    let actual=verify.tag(tag).ok_or("Staged picture tag disappeared after writing")?.pictures();
    if actual.len()!=expected.len() || actual.iter().zip(&expected).any(|(a,b)|a.data()!=b.data() || a.pic_type()!=b.pic_type() || a.description()!=b.description() || a.mime_type()!=b.mime_type()) { return Err("Audio picture verification failed".into()); }
    for old_tag in original.tags() {
        let new_tag = verify.tag(old_tag.tag_type()).ok_or("An audio tag disappeared")?;
        if !new_tag.items().into_iter().eq(old_tag.items().into_iter()) { return Err("Unrelated audio metadata changed".into()); }
        if old_tag.tag_type() == tag { continue; }
        if new_tag.pictures().len()!=old_tag.pictures().len() || new_tag.pictures().iter().zip(old_tag.pictures()).any(|(a,b)| a.data()!=b.data() || a.pic_type()!=b.pic_type() || a.description()!=b.description() || a.mime_type()!=b.mime_type()) { return Err("Another audio tag's pictures changed".into()); }
    }
    check()?;
    let now=fs::symlink_metadata(path).map_err(err)?;
    if !now.is_file() {return Err("Audio file is no longer a regular file".into());}
    if now.len()!=stamp.len() || now.modified().map_err(err)?!=stamp.modified().map_err(err)? { return Err("Audio file changed during editing".into()); }
    tmp.as_file().set_permissions(stamp.permissions()).map_err(err)?; tmp.as_file().sync_all().map_err(err)?; tmp.persist(path).map_err(err)?; Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn geometry_contract() {
        for (x,y) in [(810.,808),(1350.,1352),(562.5,560),(1084.,1088),(4.,8),(1080.,1080)] {assert_eq!(snap8(x),y);}
        let mut s=Settings::new(false); assert_eq!(geometry(&s,(2400,1800)).target,(1080,808));
        s.mode=Mode::Size;s.ratio=1;s.portrait=true;let g=geometry(&s,(2400,1800));assert_eq!(g.target,(1080,1352));assert_eq!(g.note(),"trims 361 px left and right");
        s.ratio=5;s.portrait=false;s.fill=Fill::Pad;let g=geometry(&s,(2400,1800));assert_eq!(g.target,(1080,608));assert_eq!(g.note(),"adds 134 px left and right");
        s.mode=Mode::Off;s.rotation=1;assert_eq!(geometry(&s,(2401,1801)).target,(1801,2401));
        s.rotation=0;s.square=Square::Crop;s.mode=Mode::Long;assert_eq!(geometry(&s,(1500,1426)).target,(1080,1080));
        assert!(geometry(&s,(400,300)).kept);
    }
    #[test] fn pad_upscale_tests_content_not_canvas() { let mut s=Settings::new(false);s.mode=Mode::Size;s.ratio=8;s.width=800;s.height=100;s.fill=Fill::Pad;let g=geometry(&s,(400,400));assert!(!g.kept);assert_eq!(g.resample,(104,104)); }
    #[test] fn destination_set_and_idempotency() {
        let d=tempfile::tempdir().unwrap();let p=d.path().join("cover.png");assert!(fs::read_dir(d.path()).unwrap().next().is_none());
        assert!(write_image(&d.path().join("missing/x.png"),b"a",false).is_err());assert!(fs::read_dir(d.path()).unwrap().next().is_none());
        write_image(&p,b"a",false).unwrap();assert_eq!(destination(&p,b"a",Conflict::Number).unwrap(),Destination::Unchanged(p.clone()));
        assert!(write_image(&p,b"b",false).is_err());assert_eq!(fs::read(&p).unwrap(),b"a");assert_eq!(fs::read_dir(d.path()).unwrap().count(),1);
        assert_eq!(destination(&p,b"b",Conflict::Ask).unwrap(),Destination::Conflict(p.clone()));
        write_image(&p,b"b",true).unwrap();assert_eq!(fs::read_dir(d.path()).unwrap().count(),1);
    }
    #[test]
    fn cancellation_after_staging_leaves_original_destination_set() {
        let d=tempfile::tempdir().unwrap();
        let p=d.path().join("cover.png");
        fs::write(&p,b"original").unwrap();
        let calls=std::cell::Cell::new(0);
        let check=|| { calls.set(calls.get()+1); if calls.get()>1 {Err("cancelled".into())} else {Ok(())} };
        assert!(write_image_checked(&p,b"new",true,&check).is_err());
        assert_eq!(fs::read(&p).unwrap(),b"original");
        assert_eq!(fs::read_dir(d.path()).unwrap().count(),1);
    }
    #[test]
    fn exif_orientation_is_normalized_without_relocating_other_fields() {
        let mut exif=vec![b'I',b'I',42,0,8,0,0,0,1,0,0x12,1,3,0,1,0,0,0,6,0,0,0,0,0,0,0];
        let mut expected=exif.clone(); expected[18]=1;
        normalize_exif_orientation(&mut exif).unwrap();
        assert_eq!(exif,expected);
        assert!(normalize_exif_orientation(&mut vec![0;4]).is_err());
    }
    #[test] fn preview_and_encoded_dimensions_agree() {
        let d=tempfile::tempdir().unwrap();let p=d.path().join("image.not_png"); RgbaImage::from_pixel(80,40,Rgba([70,30,5,128])).save_with_format(&p,ImageFormat::Png).unwrap();
        assert!(supported_file(&p));let input=decode(&Source::File(p)).unwrap();let mut s=Settings::new(false);s.long=32;let e=encode(&input,&s).unwrap();let out=image::load_from_memory(e.bytes.as_slice()).unwrap();assert_eq!((out.width(),out.height()),e.geometry.target);assert_eq!(e.geometry.target,(32,16));
    }
    #[test] fn png16_off_and_rotation_keep_low_order_bits(){
        let d=tempfile::tempdir().unwrap();let p=d.path().join("sixteen.png");
        let pixels=image::ImageBuffer::<Rgba<u16>,Vec<u16>>::from_fn(3,2,|x,y|Rgba([0x1200+(x+3*y) as u16,0x3401,0x5602,65535]));
        DynamicImage::ImageRgba16(pixels.clone()).save(&p).unwrap();let input=decode(&Source::File(p)).unwrap();let mut s=Settings::new(false);s.mode=Mode::Off;s.format=Format::Same;
        for rotation in [0,1]{s.rotation=rotation;let e=encode(&input,&s).unwrap();let actual=image::load_from_memory(&e.bytes).unwrap().to_rgba16();let expected=if rotation==0{pixels.clone()}else{imageops::rotate90(&pixels)};assert_eq!(actual,expected);}
    }
    #[test] fn scan_admission_and_resample_memory(){let pixels=Arc::new(DynamicImage::new_rgb8(5000,5000));let input=Decoded{pixels,format:Format::Png,bytes:0,digest:[0;32],icc:None,exif:None,extra:ExtraMetadata::default()};let s=Settings::new(false);assert!(admit((5000,5000),3).is_ok());assert!(admit_work(&input,geometry(&s,(5000,5000)),4,&s,false).is_ok());assert!(admit((32768,32768),4).is_err());}
    #[test] fn exif_cross_container_and_density(){
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("source.jpg");let mut jpeg=Vec::new();image::codecs::jpeg::JpegEncoder::new(&mut jpeg).write_image(&[100;6*4*3],6,4,image::ExtendedColorType::Rgb8).unwrap();
        let mut exif=b"Exif\0\0II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0".to_vec();insert_jpeg_metadata(&mut jpeg,jpeg_segment(0xe1,&exif).unwrap());insert_jpeg_metadata(&mut jpeg,jpeg_segment(0xfe,b"album").unwrap());fs::write(&path,jpeg).unwrap();let input=decode(&Source::File(path)).unwrap();assert_eq!((input.pixels.width(),input.pixels.height()),(4,6));let mut s=Settings::new(false);s.mode=Mode::Off;s.metadata=Metadata::All;s.format=Format::Png;let encoded=encode(&input,&s).unwrap();let metadata=container_metadata(&encoded.bytes,Format::Png).unwrap();exif=metadata.exif.unwrap();assert_eq!(exif_orientation(&exif),Some(image::metadata::Orientation::NoTransforms));assert_eq!(metadata.comments,vec![b"album".to_vec()]);
        // WebP does not carry JPEG COM, so test EXIF + density independently.
        let mut input=input;input.extra.comments.clear();input.extra.density=Some((2835,2835,true));s.format=Format::Webp;let encoded=encode(&input,&s).unwrap();let metadata=container_metadata(&encoded.bytes,Format::Webp).unwrap();assert_eq!(exif_orientation(&metadata.exif.unwrap()),Some(image::metadata::Orientation::NoTransforms));assert_eq!(image::load_from_memory(&encoded.bytes).unwrap().width(),4);
    }
    #[test] fn ordinary_bmp_and_png_color_records_keep_all(){
        let dir=tempfile::tempdir().unwrap();let mut s=Settings::new(false);s.mode=Mode::Off;s.metadata=Metadata::All;
        for format in [Format::Png,Format::Bmp]{let path=dir.path().join(format!("image.{}",format.extension()));DynamicImage::new_rgb8(12,8).save_with_format(&path,format.image()).unwrap();if format==Format::Png{let mut bytes=fs::read(&path).unwrap();bytes.splice(33..33,png_chunk(b"gAMA",&45455u32.to_be_bytes()));bytes.splice(33..33,png_chunk(b"sRGB",&[0]));fs::write(&path,bytes).unwrap();}let input=decode(&Source::File(path)).unwrap();let e=encode(&input,&s).unwrap();let metadata=container_metadata(&e.bytes,format).unwrap();assert_eq!(metadata.color,input.extra.color);assert_eq!(image::load_from_memory(&e.bytes).unwrap().height(),8);}
    }

    #[test]
    #[ignore = "large image qualification; run explicitly on the native build host"]
    fn scan_5000_to_1080_png_and_jpeg(){let dir=tempfile::tempdir().unwrap();let mut s=Settings::new(false);s.long=1080;for format in [Format::Png,Format::Jpeg]{let path=dir.path().join(format!("scan.{}",format.extension()));let pixels=image::RgbImage::from_fn(5000,5000,|x,y|image::Rgb([(x%256) as u8,(y%256) as u8,((x+y)%256) as u8]));DynamicImage::ImageRgb8(pixels).save_with_format(&path,format.image()).unwrap();let start=std::time::Instant::now();let input=decode(&Source::File(path)).unwrap();let decoded=start.elapsed();let e=encode(&input,&s).unwrap();assert_eq!(e.geometry.target,(1080,1080));assert_eq!(image::load_from_memory(&e.bytes).unwrap().width(),1080);eprintln!("{:?}: decode {:?}, encode {:?}",format,decoded,start.elapsed()-decoded);}}

    #[test]
    fn png_descriptions_survive_jpeg_and_webp_conversion() {
        let d=tempfile::tempdir().unwrap();let path=d.path().join("source.png");
        DynamicImage::new_rgb8(32,16).save(&path).unwrap();let mut png=fs::read(&path).unwrap();
        png.splice(33..33,png_chunk(b"tEXt",b"Title\0Caf\xe9 & <Music>"));
        let mut author=b"Author\0\0".to_vec();let mut zip=flate2::write::ZlibEncoder::new(Vec::new(),flate2::Compression::default());zip.write_all(b"Artist").unwrap();author.extend(zip.finish().unwrap());png.splice(33..33,png_chunk(b"zTXt",&author));
        let mut description=b"Description\0\0\0\0\0".to_vec();description.extend("Artwork \u{266a}".as_bytes());png.splice(33..33,png_chunk(b"iTXt",&description));fs::write(&path,png).unwrap();
        let input=decode(&Source::File(path)).unwrap();assert_eq!(input.extra.descriptive.len(),3);
        let mut settings=Settings::new(false);settings.mode=Mode::Off;
        for format in [Format::Jpeg,Format::Webp,Format::Png]{settings.format=format;for policy in [Metadata::All,Metadata::Icc,Metadata::Strip]{settings.metadata=policy;let output=encode(&input,&settings).unwrap();let extra=container_metadata(&output.bytes,format).unwrap();
            if policy!=Metadata::All{assert!(extra.descriptive.is_empty());assert!(extra.xmp.is_none());}
            else if format==Format::Png{for original in &input.extra.descriptive{assert!(extra.descriptive.iter().any(|field|field.raw==original.raw));}}
            else{let xmp=String::from_utf8(extra.xmp.unwrap()).unwrap();assert!(xmp.contains("Café &amp; &lt;Music&gt;"));assert!(xmp.contains("<dc:creator><rdf:Seq><rdf:li>Artist</rdf:li>"));assert!(xmp.contains("Artwork \u{266a}"));assert!(xmp.contains("<dc:description><rdf:Alt>"));}
            assert_eq!(image::load_from_memory(&output.bytes).unwrap().width(),32);
        }}
    }
    #[test]
    fn standard_xmp_dimensions_are_updated_without_losing_descriptions() {
        let d=tempfile::tempdir().unwrap();let path=d.path().join("source.jpg");DynamicImage::new_rgb8(32,16).save(&path).unwrap();let mut jpeg=fs::read(&path).unwrap();
        let packet=br#"<?xpacket begin=""?><x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:t="http://ns.adobe.com/tiff/1.0/" xmlns:e="http://ns.adobe.com/exif/1.0/" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:xmp="http://ns.adobe.com/xap/1.0/" t:ImageWidth="32" t:Orientation='6'><e:PixelXDimension>32</e:PixelXDimension><e:PixelYDimension>16</e:PixelYDimension><dc:title><rdf:Alt><rdf:li xml:lang="x-default">ImageWidth &amp; Orientation</rdf:li></rdf:Alt></dc:title><xmp:Thumbnails><rdf:Alt><rdf:li>OLD THUMBNAIL</rdf:li></rdf:Alt></xmp:Thumbnails><rdf:Description xmlns:t="urn:unrelated" t:ImageWidth="123"/></rdf:Description></rdf:RDF></x:xmpmeta><?xpacket end="w"?>"#;
        let mut body=XMP_JPEG.to_vec();body.extend(packet);insert_jpeg_metadata(&mut jpeg,jpeg_segment(0xe1,&body).unwrap());fs::write(&path,jpeg).unwrap();let input=decode(&Source::File(path)).unwrap();
        let mut settings=Settings::new(false);settings.metadata=Metadata::All;settings.long=16;
        for format in [Format::Jpeg,Format::Png,Format::Webp]{settings.format=format;for mode in [Mode::Off,Mode::Long]{settings.mode=mode;let e=encode(&input,&settings).unwrap();let dims=e.geometry.target;let extra=container_metadata(&e.bytes,format).unwrap();let xml=String::from_utf8(extra.xmp.unwrap()).unwrap();
            assert!(xml.contains(&format!("t:ImageWidth=\"{}\"",dims.0)));assert!(xml.contains("t:Orientation='1'"));assert!(xml.contains(&format!("<e:PixelXDimension>{}</e:PixelXDimension>",dims.0)));assert!(xml.contains(&format!("<e:PixelYDimension>{}</e:PixelYDimension>",dims.1)));assert!(xml.contains("ImageWidth &amp; Orientation"));assert!(xml.contains("xmlns:t=\"urn:unrelated\" t:ImageWidth=\"123\""));assert!(!xml.contains("OLD THUMBNAIL"));assert!(!xml.contains("xmp:Thumbnails"));
        }}
        settings.metadata=Metadata::Icc;assert!(container_metadata(&encode(&input,&settings).unwrap().bytes,Format::Webp).unwrap().xmp.is_none());
    }
    #[test]
    fn grayscale_icc_matches_encoded_samples() {
        // Synthetic Little CMS D50/gamma 2.2 profile. No runtime LCMS dependency.
        let profile=b"\x00\x00\x01\x5c\x6c\x63\x6d\x73\x04\x40\x00\x00\x6d\x6e\x74\x72\x47\x52\x41\x59\x58\x59\x5a\x20\x07\xea\x00\x0b\x00\x18\x00\x00\x00\x00\x00\x00\x61\x63\x73\x70\x41\x50\x50\x4c\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\xf6\xd6\x00\x01\x00\x00\x00\x00\xd3\x2d\x6c\x63\x6d\x73\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x04\x64\x65\x73\x63\x00\x00\x00\xb4\x00\x00\x00\x36\x63\x70\x72\x74\x00\x00\x00\xec\x00\x00\x00\x4c\x77\x74\x70\x74\x00\x00\x01\x38\x00\x00\x00\x14\x6b\x54\x52\x43\x00\x00\x01\x4c\x00\x00\x00\x10\x6d\x6c\x75\x63\x00\x00\x00\x00\x00\x00\x00\x01\x00\x00\x00\x0c\x65\x6e\x55\x53\x00\x00\x00\x1a\x00\x00\x00\x1c\x00\x67\x00\x72\x00\x61\x00\x79\x00\x20\x00\x62\x00\x75\x00\x69\x00\x6c\x00\x74\x00\x2d\x00\x69\x00\x6e\x00\x00\x6d\x6c\x75\x63\x00\x00\x00\x00\x00\x00\x00\x01\x00\x00\x00\x0c\x65\x6e\x55\x53\x00\x00\x00\x30\x00\x00\x00\x1c\x00\x4e\x00\x6f\x00\x20\x00\x63\x00\x6f\x00\x70\x00\x79\x00\x72\x00\x69\x00\x67\x00\x68\x00\x74\x00\x2c\x00\x20\x00\x75\x00\x73\x00\x65\x00\x20\x00\x66\x00\x72\x00\x65\x00\x65\x00\x6c\x00\x79\x58\x59\x5a\x20\x00\x00\x00\x00\x00\x00\xf6\xd6\x00\x01\x00\x00\x00\x00\xd3\x2d\x70\x61\x72\x61\x00\x00\x00\x00\x00\x00\x00\x00\x00\x02\x33\x33".to_vec();
        let d=tempfile::tempdir().unwrap();let path=d.path().join("gray.png");let mut bytes=Vec::new();let mut encoder=image::codecs::png::PngEncoder::new(&mut bytes);encoder.set_icc_profile(profile.clone()).unwrap();
        let samples=image::ImageBuffer::<image::Luma<u16>,Vec<u16>>::from_fn(4,2,|x,y|image::Luma([0x1200+(x+y*4) as u16]));
        encoder.write_image(DynamicImage::ImageLuma16(samples.clone()).as_bytes(),4,2,image::ExtendedColorType::L16).unwrap();fs::write(&path,bytes).unwrap();let input=decode(&Source::File(path)).unwrap();assert_eq!(input.icc.as_deref(),Some(profile.as_slice()));
        let mut settings=Settings::new(false);settings.mode=Mode::Off;settings.rotation=1;
        for policy in [Metadata::All,Metadata::Icc]{settings.metadata=policy;let e=encode(&input,&settings).unwrap();assert_eq!(e.bytes[24],16);assert_eq!(e.bytes[25],4); // independent PNG IHDR sample depth/type
            let mut reader=ImageReader::with_format(Cursor::new(e.bytes.as_slice()),ImageFormat::Png).into_decoder().unwrap();assert_eq!(reader.icc_profile().unwrap().as_deref(),Some(profile.as_slice()));let p=DynamicImage::from_decoder(reader).unwrap();assert_eq!(p.color(),image::ColorType::La16);let expected=imageops::rotate90(&samples);assert_eq!(p.to_luma16(),expected);
            settings.square=Square::Pad;settings.pad=Some([255,0,0,255]);assert!(encode(&input,&settings).unwrap_err().contains("grayscale ICC"));settings.square=Square::Off;settings.pad=None;
            settings.format=Format::Webp;assert!(encode(&input,&settings).unwrap_err().contains("grayscale ICC"));settings.format=Format::Same;
        }
        settings.metadata=Metadata::Strip;let e=encode(&input,&settings).unwrap();let mut reader=ImageReader::with_format(Cursor::new(e.bytes.as_slice()),ImageFormat::Png).into_decoder().unwrap();assert!(reader.icc_profile().unwrap().is_none());
    }
    #[test]
    fn obsolete_resampling_stops_during_each_pass() {
        let p=RgbaImage::from_pixel(64,64,Rgba([20,40,60,255]));let mut settings=Settings::new(false);
        // Two weight checkpoints, initial admission, 64 first-pass rows,
        // then 32 second-pass rows. Cancellation stops at the first check.
        for (filter,boundary) in [(0,8),(0,70),(1,8),(2,4)]{settings.filter=filter;let calls=std::cell::Cell::new(0usize);let check=||{calls.set(calls.get()+1);if calls.get()>=boundary{Err("cancelled".into())}else{Ok(())}};assert_eq!(resize(&p,(32,32),&settings,&check).unwrap_err(),"cancelled");assert_eq!(calls.get(),boundary);}
        settings.filter=0;let calls=std::cell::Cell::new(0usize);let actual=resize(&p,(32,32),&settings,&||{calls.set(calls.get()+1);Ok(())}).unwrap();assert_eq!(actual.dimensions(),(32,32));assert!(calls.get()<110); // no per-pixel atomic polling
    }

}
