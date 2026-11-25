//! Fixed-geometry Edit Image form. All controls are described once, with their
//! visual spans also serving as focus/navigation and mouse hit targets.
use super::{app::AppState, artwork_settings as ui, browse_artwork::{BrowseArtworkState, ImageVerticalPlacement}, display_width as dw, edit_image_pipeline as pipe, theme::Theme, text_input::{TextInputState, handle_text_input_key}};
use pipe::{Conflict, Decoded, Destination, Encoded, Fill, Format, Metadata, Mode, Settings, Source, Square, WriteMode};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Frame, layout::Rect, style::Color, widgets::Clear};
use std::{path::{Path,PathBuf}, sync::{Arc,Mutex,mpsc,atomic::{AtomicBool,Ordering}}, thread, time::{Duration,Instant}};
use tui_file_picker::{ConflictItemKind, ConflictPromptState, ConflictResolution, FilePickerAction, FilePickerState, FileTaskKind, FileTaskPhase, FileTaskProgressState, FileTaskProgressUpdate, FileTaskUserAction, ProgressItem, ProgressTotals};

const WIDTH:u16=76;
const MODES:[Mode;5]=[Mode::Off,Mode::Scale,Mode::Percent,Mode::Long,Mode::Size];
const MODE_NAMES:[&str;5]=["Off","Scale","Percent","Long edge","Size"];
const FILTERS:[&str;3]=["Lanczos3","CatmullRom","Nearest"];
const META:[Metadata;3]=[Metadata::Strip,Metadata::Icc,Metadata::All];
const META_NAMES:[&str;3]=["Strip all","Keep ICC only","Keep all"];
const FILLS:[Fill;3]=[Fill::Crop,Fill::Pad,Fill::Stretch];
const FILL_NAMES:[&str;3]=["Crop to fill","Pad","Stretch"];
const WRITES:[WriteMode;3]=[WriteMode::File,WriteMode::Embed,WriteMode::Both];
const WRITE_NAMES:[&str;3]=["File","Re-embed","File + re-embed"];
const CONFLICTS:[Conflict;4]=[Conflict::Ask,Conflict::Replace,Conflict::Skip,Conflict::Number];
const CONFLICT_NAMES:[&str;4]=["Ask","Replace it","Skip it","Add a number"];
const ROTATIONS:[&str;4]=["None","90° right","180°","90° left"];
const SQUARES:[Square;3]=[Square::Off,Square::Crop,Square::Pad];
const SQUARE_NAMES:[&str;3]=["Off","Center crop","Pad"];
const PRESETS:[[f64;4];3]=[[1.5,2.,3.,4.],[75.,50.,25.,12.5],[480.,720.,1080.,1440.]];
const PRESET_NAMES:[[&str;4];3]=[["1.5×","2×","3×","4×"],["75%","50%","25%","12.5%"],["480","720","1080","1440"]];

#[derive(Clone,Debug)]
struct Pending<T>(Arc<Mutex<mpsc::Receiver<T>>>);
impl<T> Pending<T> {
    fn spawn(f:impl FnOnce()->T+Send+'static)->Self where T:Send+'static { let(tx,rx)=mpsc::channel();thread::spawn(move||{let _=tx.send(f());});Self(Arc::new(Mutex::new(rx))) }
    fn poll(&self)->Result<T,mpsc::TryRecvError> { self.0.lock().unwrap_or_else(|e|e.into_inner()).try_recv() }
}
#[derive(Clone,Debug)]
struct Loaded { primary:Vec<Source>, alternate:Option<Vec<Source>>, index:usize, focused:Option<Decoded> }
#[derive(Clone,Debug)]
struct Work { revision:u64, source:Source, result:Result<(Decoded,Result<Encoded,String>),String> }
#[derive(Debug)]
struct EncodeLease(Arc<AtomicBool>);
impl Drop for EncodeLease {fn drop(&mut self){self.0.store(true,Ordering::Release);}}
#[derive(Clone,Debug)]
struct EncodeWorker {pending:Pending<Work>,lease:Arc<EncodeLease>,revision:u64}
impl EncodeWorker {
    fn spawn(source:Source,cached:Option<Decoded>,settings:Settings,revision:u64)->Self{
        let cancel=Arc::new(AtomicBool::new(false));let lease=Arc::new(EncodeLease(cancel.clone()));
        let pending=Pending::spawn(move||{
            let check=||->Result<(),String>{if cancel.load(Ordering::Acquire){Err("Obsolete image plan cancelled".into())}else{Ok(())}};
            let result=check().and_then(|()|cached.map(Ok).unwrap_or_else(||pipe::decode(&source))).map(|d|{let e=pipe::encode_checked(&d,&settings,&check);(d,e)});
            Work{revision,source,result}
        });
        Self{pending,lease,revision}
    }
    fn cancel(&self){self.lease.0.store(true,Ordering::Release);}
}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
enum Target { Picker, Image, Transform, Mode(usize), Preset(usize), Custom, W,H,Ratio,A,B,Orientation(bool),Fill,Pad,Filter,Linear,Upscale,Format,Quality,QualityBar,Metadata,Conflict,Write,Template,Browse,Ok,Preview,Cancel,Help,Rotate,FlipH,FlipV,Square,Done,SourceChoice,AlternateChoice,ColorField,ColorDone,ColorDefault,ColorChannel(usize),ColorStep(usize,bool),TemplateField,Token(usize),Separator(usize),TemplateLeft,TemplateRight,TemplateBackspace,TemplateClear,TemplateApply,TemplateCancel }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
struct Hit { target:Target, x:u16,y:u16,w:u16, band:u8 }
impl Hit { fn new(target:Target,x:u16,y:u16,w:u16,band:u8)->Self {Self{target,x,y,w,band}} fn contains(self,x:u16,y:u16)->bool {self.y==y&&x>=self.x&&x<self.x+self.w} }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
enum Popup { Transform,Source,Color,Template,Help }
#[derive(Clone,Debug)]
struct FieldEdit { target:Target, input:TextInputState, prior_preset:Option<(usize,usize)> }
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
enum Request { Ok,Preview }
#[derive(Clone)]
struct Preview { viewer:Arc<Mutex<BrowseArtworkState>>, before:Arc<image::DynamicImage>, after:Option<Arc<image::DynamicImage>>, pending:Option<Pending<Result<image::DynamicImage,String>>>, show_after:bool, show_help:bool, error:Option<String> }
impl std::fmt::Debug for Preview {fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.debug_struct("EditImagePreview").field("show_after",&self.show_after).finish_non_exhaustive()} }
impl Preview {
    fn new(before:Arc<image::DynamicImage>,encoded:&Encoded,background:Color)->Self {let bytes=Arc::clone(&encoded.bytes); let (r,g,b)=super::theme::rgb_tuple(background); Self{viewer:Arc::new(Mutex::new(BrowseArtworkState::default())),before,after:None,pending:Some(Pending::spawn(move||image::load_from_memory(bytes.as_slice()).map(|p|super::browse_artwork::composite_viewer_alpha(p,[r,g,b])).map_err(|e|e.to_string()))),show_after:true,show_help:false,error:None} }
    fn display(&mut self,background:Color) {let p=if self.show_after {self.after.as_ref()} else {Some(&self.before)};if let Some(p)=p {let pixels=if p.color().has_alpha()&&!self.show_after{let(r,g,b)=super::theme::rgb_tuple(background);Arc::new(super::browse_artwork::composite_viewer_alpha(p.as_ref().clone(),[r,g,b]))}else{Arc::clone(p)};self.viewer.lock().unwrap_or_else(|e|e.into_inner()).set_edit_preview_pixels(pixels,background);} }
    fn toggle(&mut self,after:bool,background:Color) {if self.show_after!=after {self.show_after=after;self.display(background);} }
    fn prepare(&mut self,picker:&mut ratatui_image::picker::Picker,generation:usize,retransmit:usize,background:Color)->bool {
        let mut changed=false;
        if let Some(p)=self.pending.as_ref() {match p.poll(){Ok(Ok(p))=>{self.after=Some(Arc::new(p));self.pending=None;if self.show_after {self.display(background);}changed=true;},Ok(Err(e))=>{self.error=Some(e);self.pending=None;changed=true;},Err(mpsc::TryRecvError::Disconnected)=>{self.error=Some("Preview worker exited".into());self.pending=None;changed=true;},Err(mpsc::TryRecvError::Empty)=>{}}}
        changed|self.viewer.lock().unwrap_or_else(|e|e.into_inner()).prepare(picker,generation,retransmit,&crate::config::BrowseArtworkConfig::default(),background)
    }
    fn draw(&mut self,f:&mut Frame,theme:Theme) {let screen=f.size();f.render_widget(Clear,screen);ui::fill(f,screen,theme.bg);if screen.height<2 {return;}if self.show_help {let area=centered(screen,40,7);if area.width>0{ui::fixed_frame(f,area,"All keys",&[],theme);ui::paint(f,area,3,2,"Space  Before / After",theme.value,theme.panel_bg);ui::paint(f,area,3,3,"Esc    Return to preview",theme.value,theme.panel_bg);ui::paint(f,area,3,5," Done ",theme.panel_bg,theme.chip_go);}return;}let area=Rect::new(screen.x,screen.y,screen.width,screen.height-1);let mut viewer=self.viewer.lock().unwrap_or_else(|e|e.into_inner());if viewer.take_background_restore(){super::browse_artwork::repaint_image_background(f,area,theme.bg,viewer.background_restore_count());}let shown=(!self.show_after||self.after.is_some())&&viewer.render_image(f,area,false,ImageVerticalPlacement::Center,theme.bg);if !shown {let message=self.error.as_deref().unwrap_or("Preparing preview…");ui::paint(f,screen,screen.width.saturating_sub(dw::width(message) as u16)/2,area.height/2,message,theme.value,theme.bg);}let y=screen.height-1;let x=screen.width.saturating_sub(22)/2;ui::paint(f,screen,x,y,if self.show_after {"● After    ○ Before"}else{"○ After    ● Before"},theme.info,theme.bg);ui::paint(f,screen,screen.width.saturating_sub(10),y,"? all keys",theme.info,theme.bg);}
}

const SKIP_CURRENT: &str = "Current image skipped";
const CANCELLED: &str = "Image edit cancelled";
type CancelFlag = super::probe::MetadataWriteCancelFlag;

#[derive(Clone, Debug, Default)]
struct SkipGate(Arc<Mutex<Option<(usize, bool)>>>);
impl SkipGate {
    fn begin(&self, index: usize) { *self.0.lock().unwrap_or_else(|e|e.into_inner()) = Some((index,false)); }
    fn request(&self, index: usize) -> bool {
        let mut state = self.0.lock().unwrap_or_else(|e|e.into_inner());
        if let Some((active, requested)) = state.as_mut() {
            if *active == index { *requested = true; return true; }
        }
        false
    }
    fn check(&self, index: usize) -> Result<(),String> {
        if *self.0.lock().unwrap_or_else(|e|e.into_inner()) == Some((index,true)) { Err(SKIP_CURRENT.into()) } else { Ok(()) }
    }
    // Last cancellable boundary for this item's first publication. Request
    // and seal are serialized; a late request can never affect a later item.
    fn seal(&self, index: usize) -> Result<(),String> {
        let mut state = self.0.lock().unwrap_or_else(|e|e.into_inner());
        if *state == Some((index,true)) { return Err(SKIP_CURRENT.into()); }
        if state.as_ref().is_some_and(|(active,_)| *active == index) { *state = None; }
        Ok(())
    }
    fn end(&self, index: usize) {
        let mut state = self.0.lock().unwrap_or_else(|e|e.into_inner());
        if state.as_ref().is_some_and(|(active,_)| *active == index) { *state = None; }
    }
}
#[derive(Clone,Copy,Debug,Default)]
struct JobEffects { files_written:u64, tracks_updated:u64, cleanup_pending:u64 }
impl JobEffects {fn changed(self)->bool{self.files_written>0||self.tracks_updated>0}}
#[derive(Clone,Debug)]
enum JobEvent { Begin(usize,FileTaskProgressUpdate), Update(FileTaskProgressUpdate), Done(Result<String,JobFailure>,ProgressTotals,JobEffects) }
#[derive(Clone,Debug)]
struct JobFailure {status:String,aborted:bool}
impl From<String> for JobFailure {fn from(status:String)->Self{Self{status,aborted:false}}}
#[derive(Clone,Debug)]
struct JobLease(Arc<CancelFlag>);
impl Drop for JobLease { fn drop(&mut self){self.0.cancel();} }
#[derive(Clone,Debug)]
struct Job { _lease:Arc<JobLease>, events:Pending<JobEvent>, response:mpsc::Sender<(usize,ConflictResolution)>, cancel:Arc<CancelFlag>, pause:Arc<AtomicBool>, skip:SkipGate, displayed:Option<usize>, progress:FileTaskProgressState }
impl Job {
    fn start(sources:Vec<Source>,focused:usize,decoded:Decoded,encoded:Encoded,settings:Settings,theme:Theme)->Self {
        let (tx,rx)=mpsc::channel();let (answer,response_rx)=mpsc::channel();let cancel=Arc::new(CancelFlag::new());let pause=Arc::new(AtomicBool::new(false));let skip=SkipGate::default();let flags=(cancel.clone(),pause.clone(),skip.clone());
        let total=sources.len() as u64;
        thread::spawn(move||{
            let mut totals=ProgressTotals{items_total:Some(total),..Default::default()};
            let mut effects=JobEffects::default();
            let result=run_job(&sources,focused,&decoded,&encoded,&settings,&tx,&response_rx,&flags,&mut totals,&mut effects);
            let _=tx.send(JobEvent::Done(result,totals,effects));
        });
        let mut progress=FileTaskProgressState::new(FileTaskKind::Custom("Edit image".into()),"Edit image",super::keybindings::file_picker_theme_from_theme(&theme));progress.totals.items_total=Some(total);progress.set_task_controls_available(true);
        Self{_lease:Arc::new(JobLease(cancel.clone())),events:Pending(Arc::new(Mutex::new(rx))),response:answer,cancel,pause,skip,displayed:None,progress}
    }
    fn action(&mut self,a:FileTaskUserAction)->bool {match a {
        FileTaskUserAction::Abort=>{self.cancel.cancel();self.pause.store(false,Ordering::Release);self.progress.phase=FileTaskPhase::Cancelling;},
        FileTaskUserAction::Pause=>{self.pause.store(true,Ordering::Release);self.progress.phase=FileTaskPhase::Paused;},FileTaskUserAction::Resume=>{self.pause.store(false,Ordering::Release);self.progress.phase=FileTaskPhase::Running;},
        FileTaskUserAction::SkipCurrent=>{if let Some(index)=self.displayed {if self.skip.request(index) {self.pause.store(false,Ordering::Release);}else{self.progress.status="Current image has passed its last safe skip point".into();}}},
        FileTaskUserAction::ChooseConflictResolution(r)=>{if let Some(index)=self.displayed {let _=self.response.send((index,r));}self.progress.clear_conflict();},FileTaskUserAction::Acknowledge=>return false,_=>{} }true}
}
fn run_job(sources:&[Source],focused:usize,decoded:&Decoded,encoded:&Encoded,settings:&Settings,tx:&mpsc::Sender<JobEvent>,answers:&mpsc::Receiver<(usize,ConflictResolution)>,flags:&(Arc<CancelFlag>,Arc<AtomicBool>,SkipGate),totals:&mut ProgressTotals,effects:&mut JobEffects)->Result<String,JobFailure> {
    let pending = std::cell::Cell::new(0u64);
    let report = |_| pending.set(pending.get().saturating_add(1));
    let mut result = run_job_with_reembed(sources,focused,decoded,encoded,settings,tx,answers,flags,totals,effects,&report,&|source,e,other,check,cancel|pipe::reembed_checked_reporting(source,e,other,settings.working_root.as_deref(),&||check(),cancel,&report));
    effects.cleanup_pending = pending.get();
    if effects.cleanup_pending != 0 {
        // Keep the actual publication result and partial-completion counts.
        // Cleanup failure must not misreport a successfully published file.
        let warning = "; temporary staging cleanup pending (Interrupted jobs)";
        match &mut result {
            Ok(status) => status.push_str(warning),
            Err(failure) => failure.status.push_str(warning),
        }
    }
    result
}
// The injected writer boundary permits deterministic testing after file
// publication without sleeps or test-only branches in the native writers.
fn run_job_with_reembed(sources:&[Source],focused:usize,decoded:&Decoded,encoded:&Encoded,settings:&Settings,tx:&mpsc::Sender<JobEvent>,answers:&mpsc::Receiver<(usize,ConflictResolution)>,flags:&(Arc<CancelFlag>,Arc<AtomicBool>,SkipGate),totals:&mut ProgressTotals,effects:&mut JobEffects,report:&impl Fn(String),reembed:&impl Fn(&Source,&Encoded,Option<&Path>,&dyn Fn()->Result<(),String>,&CancelFlag)->Result<bool,String>)->Result<String,JobFailure> {
    let(cancel,pause,skip)=flags;
    let total=sources.len() as u64;totals.items_total=Some(total);totals.not_attempted=total;
    let mut policy=settings.conflict;let mut last=None;let mut embed_completed=false;
    let source_paths:Vec<_>=sources.iter().map(|s|std::fs::canonicalize(s.path()).unwrap_or_else(|_|s.path().to_path_buf())).collect();
    for (index,source) in sources.iter().enumerate(){
        if cancel.is_cancelled(){return Err(JobFailure{status:CANCELLED.into(),aborted:true});}
        skip.begin(index);
        let check=||->Result<(),String>{while pause.load(Ordering::Acquire)&&!cancel.is_cancelled(){skip.check(index)?;thread::sleep(Duration::from_millis(40));}if cancel.is_cancelled(){Err(CANCELLED.into())}else{skip.check(index)}};
        let commit=||->Result<(),String>{check()?;skip.seal(index)};
        let mut item=ProgressItem::new(source.label());item.source=Some(source.path().to_path_buf());
        let send=|phase,status:&str,item:Option<ProgressItem>,totals|tx.send(JobEvent::Update(FileTaskProgressUpdate::Snapshot{phase,status:status.into(),current_item:item,totals,rate_bytes_per_sec:None})).map_err(|_|"Image editor closed".to_string());
        tx.send(JobEvent::Begin(index,FileTaskProgressUpdate::Snapshot{phase:FileTaskPhase::Running,status:"Encoding image".into(),current_item:Some(item.clone()),totals:*totals,rate_bytes_per_sec:None})).map_err(|_|"Image editor closed".to_string())?;
        let mut image_ready:Option<(PathBuf,bool)>=None;
        let do_embed=settings.write!=WriteMode::File && source.embedded();
        let outcome=(||->Result<bool,String>{
            check()?;
            let e=if index==focused {if !pipe::source_matches(source,decoded.digest)? {return Err("Source image changed; reopen Edit Image".into());}encoded.clone()}else{pipe::encode_checked(&pipe::decode(source)?,settings,&check)?};
            check()?;
            if !source.embedded()||settings.write!=WriteMode::Embed {
                let path=pipe::output_path(source,&e,&settings.template)?;
                let identity=std::fs::canonicalize(&path).unwrap_or_else(|_|path.clone());
                if source_paths.iter().enumerate().any(|(i,p)|*p==identity&&(i!=index||source.embedded())) {return Err(format!("Output would replace a different source: {}",path.display()));}
                let mut dest=pipe::destination(&path,e.bytes.as_slice(),policy)?;
                if let Destination::Conflict(p)=&dest {
                    let mut prompt=ConflictPromptState::new(index as u64+1,"Image exists","Choose what to do with the existing image",ConflictItemKind::File);prompt.source=Some(source.path().to_path_buf());prompt.destination=Some(p.clone());prompt.incoming_size=Some(e.bytes.len() as u64);prompt.existing_size=std::fs::metadata(p).ok().map(|m|m.len());
                    tx.send(JobEvent::Update(FileTaskProgressUpdate::ShowConflict{conflict:prompt})).map_err(|_|"Image editor closed".to_string())?;
                    let choice=loop{check()?;match answers.recv_timeout(Duration::from_millis(100)){Ok((id,c)) if id==index=>break c,Ok(_)=>{},Err(mpsc::RecvTimeoutError::Timeout)=>{},Err(mpsc::RecvTimeoutError::Disconnected)=>return Err("Image editor closed".into())}};
                    let selected=match choice{ConflictResolution::Overwrite{apply_to_all}=>{if apply_to_all{policy=Conflict::Replace;}Conflict::Replace},ConflictResolution::Skip{apply_to_all}=>{if apply_to_all{policy=Conflict::Skip;}Conflict::Skip},ConflictResolution::Rename=>Conflict::Number,ConflictResolution::Abort=>{cancel.cancel();return Err(CANCELLED.into());}};
                    dest=pipe::destination(&path,e.bytes.as_slice(),selected)?;tx.send(JobEvent::Update(FileTaskProgressUpdate::ClearConflict)).map_err(|_|"Image editor closed".to_string())?;
                }
                check()?;
                match dest {Destination::Write(p,replace)=>{item.destination=Some(p.clone());item.bytes_total=Some(e.bytes.len() as u64);send(FileTaskPhase::Running,"Writing image",Some(item.clone()),*totals)?;let published=pipe::write_image_with_commit_reporting(&p,e.bytes.as_slice(),replace,Some(source.path()),settings.working_root.as_deref(),&check,&commit,report)?;if published{effects.files_written+=1;}else if !do_embed{commit()?;}image_ready=Some((p.clone(),published));last=Some(p);item.bytes_done=item.bytes_total.unwrap_or(0);},Destination::Unchanged(p)=>{if !do_embed{commit()?;}item.destination=Some(p.clone());image_ready=Some((p.clone(),false));last=Some(p);},Destination::Skip=>{return Ok(false);},Destination::Conflict(_)=>return Err("Unresolved image conflict".into())}
            }
            if do_embed {commit()?;send(FileTaskPhase::Running,"Re-embedding artwork",Some(item.clone()),*totals)?;if reembed(source,&e,image_ready.as_ref().map(|(p,_)|p.as_path()),&check,cancel)?{effects.tracks_updated+=1;}embed_completed=true;}
            // Unchanged and conflict-policy skips have no irreversible write.
            // Seal before reporting a completed result.
            skip.seal(index)?;
            Ok(true)
        })();
        skip.end(index);
        // A skip/cancel while the prompt is displayed must dismiss that prompt;
        // conflict replies are occurrence-scoped and cannot leak to the next item.
        let _=tx.send(JobEvent::Update(FileTaskProgressUpdate::ClearConflict));
        match outcome {
            Ok(true)=>{totals.completed+=1;item.bytes_done=item.bytes_total.unwrap_or(0);},
            Ok(false)=>{totals.skipped+=1;},
            Err(e) if e==SKIP_CURRENT=>{totals.skipped+=1;},
            Err(e)=>{
                // This item was reached, even if only its first requested
                // output was published. errors counts the incomplete item,
                // including an attempted item aborted by user cancellation.
                totals.errors+=1;totals.items_done+=1;totals.not_attempted=total.saturating_sub(totals.items_done);
                let aborted=e==CANCELLED||(cancel.is_cancelled()&&e.contains("metadata save cancelled")&&!e.contains("restore failed")&&!e.contains("restoration failed")&&!e.contains("remains armed"));
                let status=if let Some((path,published))=image_ready {
                    let image=if published{"Written"}else{"Already up to date"};
                    let reembed=if aborted{"Re-embed cancelled"}else{"Re-embed failed"};
                    format!("{image}: {}; {reembed}: {e}",path.display())
                }else{format!("{}: {e}",source.path().display())};
                let mut record=tui_file_picker::FileTaskErrorRecord::new(source.label(),status.clone());record.source=Some(source.path().to_path_buf());record.destination=item.destination.clone();
                let _=tx.send(JobEvent::Update(FileTaskProgressUpdate::RecordError{error:record,totals:*totals}));
                return Err(JobFailure{status,aborted});
            },
        }
        totals.items_done+=1;totals.not_attempted=total.saturating_sub(totals.items_done);
        send(FileTaskPhase::Running,"Image processed",Some(item),*totals)?;
    }
    let written=effects.files_written;let tracks=effects.tracks_updated;
    Ok(if sources.len()==1 {match last {Some(p) if written==0&&tracks==0=>format!("Already up to date: {}",p.display()),Some(p) if tracks>0=>format!("Image ready: {}; artwork re-embedded",p.display()),Some(p)=>format!("Written {}",p.display()),None if tracks>0=>"Artwork re-embedded".into(),None if embed_completed=>"Artwork already up to date".into(),None=>"Image skipped".into()}}else{format!("Image edit complete: {written} files written, {tracks} tracks updated, {} skipped",totals.skipped)})
}

fn discover_files(clicked:PathBuf,selection:Vec<PathBuf>)->Result<Loaded,String> {
    let focused=pipe::decode(&Source::File(clicked.clone())).map_err(|e|format!("Cannot edit {}: {e}",clicked.display()))?;
    let mut paths=Vec::new();
    if selection.contains(&clicked) {
        for p in selection {
            if !paths.contains(&p) && (p==clicked || (pipe::supported_file(&p) && pipe::decode(&Source::File(p.clone())).is_ok())) { paths.push(p); }
        }
    }
    if paths.len()<2 {return Ok(Loaded{primary:vec![Source::File(clicked)],alternate:None,index:0,focused:Some(focused)});}
    let index=paths.iter().position(|p|p==&clicked).expect("clicked image was validated");
    Ok(Loaded{primary:paths.into_iter().map(Source::File).collect(),alternate:Some(vec![Source::File(clicked)]),index,focused:Some(focused)})
}

#[derive(Clone,Debug)]
pub struct EditImageState {
    embedded:bool, clicked:PathBuf, sources:Vec<Source>, alternate:Option<Vec<Source>>, index:usize,
    settings:Settings, presets:[usize;3], customs:[f64;3], focus:Target, hover:Option<Target>,
    area:Rect, popup_area:Rect, hits:Vec<Hit>, popup:Option<Popup>, popup_return:Target, help_return:Option<(Popup,Target)>,
    edit:Option<FieldEdit>, folder:Option<FilePickerState>, color_return:Option<Popup>, color_initial:[u8;4], color_input:TextInputState, template_input:TextInputState,
    load:Option<Pending<Result<Loaded,String>>>, work:Option<EncodeWorker>, decoded:Option<(Source,Decoded)>, encoded:Option<Encoded>, revision:u64, changed:Instant,
    problem:Option<String>, request:Option<Request>, preview:Option<Preview>, job:Option<Job>,
    pub(super) finished:Option<String>, pub(super) wrote:bool,
    pub(super) cleanup_pending:bool,
}
impl EditImageState {
    pub(super) fn files(clicked:PathBuf,selection:Vec<PathBuf>)->Self {
        let click=clicked.clone();let load=Pending::spawn(move||discover_files(click,selection));Self::new(clicked,false,load)
    }
    pub(super) fn artwork(path:PathBuf,embedded:bool)->Self {if !embedded{return Self::files(path,Vec::new());}let p=path.clone();Self::new(path,true,Pending::spawn(move||pipe::pictures(&p).map(|(primary,index)|Loaded{primary,alternate:None,index,focused:None})))}
    fn new(clicked:PathBuf,embedded:bool,load:Pending<Result<Loaded,String>>)->Self {Self{embedded,clicked,sources:Vec::new(),alternate:None,index:0,settings:Settings::new(embedded),presets:[1,1,2],customs:[2.,50.,1080.],focus:Target::Mode(3),hover:None,area:Rect::default(),popup_area:Rect::default(),hits:Vec::new(),popup:None,popup_return:Target::Transform,help_return:None,edit:None,folder:None,color_return:None,color_initial:[0,0,0,0],color_input:TextInputState::empty(),template_input:TextInputState::empty(),load:Some(load),work:None,decoded:None,encoded:None,revision:0,changed:Instant::now(),problem:None,request:None,preview:None,job:None,finished:None,wrote:false,cleanup_pending:false}}
    pub(super) fn with_working_root(mut self, root: Option<PathBuf>) -> Self {
        self.settings.working_root = root;
        self
    }
    fn source(&self)->Option<&Source>{self.sources.get(self.index)}
    fn current(&self)->Option<&Decoded>{self.decoded.as_ref().filter(|(src,_)|Some(src)==self.source()).map(|(_,d)|d)}
    fn geometry(&self)->Option<pipe::Geometry>{self.current().map(|d|pipe::geometry(&self.settings,(d.pixels.width(),d.pixels.height())))}
    fn requested_geometry(&self)->Option<pipe::Geometry>{let mut requested=self.settings.clone();requested.no_upscale=false;self.current().map(|d|pipe::geometry(&requested,(d.pixels.width(),d.pixels.height())))}
    fn format(&self)->Format{self.settings.resolved_format(self.current().map(|d|d.format).unwrap_or(Format::Png))}
    fn quality(&self)->u8{self.settings.quality_for(self.format())}
    fn set_quality(&mut self,value:u8){let format=self.format();self.settings.set_quality_for(format,value);}
    fn files_visible(&self)->bool{!self.embedded||self.settings.write!=WriteMode::Embed}
    fn height(&self)->u16{if self.embedded{22}else{21}}
    fn preset_group(&self)->Option<usize>{match self.settings.mode{Mode::Scale=>Some(0),Mode::Percent=>Some(1),Mode::Long=>Some(2),_=>None}}
    fn update_preset(&mut self,g:usize){let n=self.presets[g];let v=if n==4{self.customs[g]}else{PRESETS[g][n]};match g{0=>self.settings.scale=v,1=>self.settings.percent=v,_=>self.settings.long=pipe::snap8(v)}}
    fn dirty(&mut self){if let Some(work)=&self.work{work.cancel();}self.revision=self.revision.wrapping_add(1);self.changed=Instant::now();self.encoded=None;if !self.sources.is_empty(){self.problem=None;}self.request=None;}
    fn cycle_source(&mut self,d:i32){if self.sources.len()>1{self.index=step(self.index,self.sources.len(),d);self.decoded=None;self.dirty();}}
    fn switch_source(&mut self){if let Some(mut other)=self.alternate.take(){std::mem::swap(&mut self.sources,&mut other);self.alternate=Some(other);self.index=self.sources.iter().position(|s|s.path()==self.clicked).unwrap_or(0);self.decoded=None;self.dirty();}self.popup=None;self.focus=Target::Picker;}
    fn formats(&self)->Vec<Format>{if self.embedded&&self.settings.write!=WriteMode::File{vec![Format::Jpeg,Format::Png]}else{Format::ALL.to_vec()}}
    fn cycle(&mut self,t:Target,d:i32){match t {
        Target::Image=>{self.cycle_source(d);return;},Target::Ratio=>self.settings.ratio=step(self.settings.ratio,9,d),
        Target::Fill=>self.settings.fill=cycle_value(&FILLS,self.settings.fill,d),Target::Filter=>self.settings.filter=step(self.settings.filter,FILTERS.len(),d),
        Target::Format=>{self.settings.format=cycle_value(&self.formats(),self.settings.format,d);},Target::Quality=>self.set_quality((i32::from(self.quality())+5*d).clamp(1,100) as u8),
        Target::Metadata=>self.settings.metadata=cycle_value(&META,self.settings.metadata,d),Target::Conflict=>self.settings.conflict=cycle_value(&CONFLICTS,self.settings.conflict,d),
        Target::Write=>{self.settings.write=cycle_value(&WRITES,self.settings.write,d);if self.settings.write!=WriteMode::File&&!matches!(self.settings.format,Format::Jpeg|Format::Png){self.settings.format=Format::Jpeg;}},
        Target::Rotate=>self.settings.rotation=step(self.settings.rotation,4,d),Target::Square=>self.settings.square=cycle_value(&SQUARES,self.settings.square,d),_=>{return;} }self.dirty();}
    fn is_cycler(t:Target)->bool{matches!(t,Target::Image|Target::Ratio|Target::Fill|Target::Filter|Target::Format|Target::Quality|Target::Metadata|Target::Conflict|Target::Write|Target::Rotate|Target::Square)}
    fn number(t:Target)->bool{matches!(t,Target::Custom|Target::W|Target::H|Target::A|Target::B|Target::Quality)}
    fn numeric_text(&self,t:Target)->String{match t{Target::Custom=>self.preset_group().map(|g|self.customs[g].to_string()).unwrap_or_default(),Target::W=>self.settings.width.to_string(),Target::H=>if self.settings.ratio==8{self.settings.height.to_string()}else{self.requested_geometry().map(|g|g.target.1).unwrap_or(self.settings.height).to_string()},Target::A=>self.settings.ratio_terms().unwrap_or(self.settings.custom_ratio).0.to_string(),Target::B=>self.settings.ratio_terms().unwrap_or(self.settings.custom_ratio).1.to_string(),Target::Quality=>self.quality().to_string(),_=>String::new()}}
    fn start_field(&mut self,t:Target,digit:Option<char>){
        let prior_preset=if t==Target::Custom {self.preset_group().and_then(|g|{
            let previous=self.presets[g];if previous==4{return None;}
            self.presets[g]=4;self.update_preset(g);self.dirty();Some((g,previous))
        })}else{None};
        let text=self.numeric_text(t);let mut input=TextInputState::new_selected(text);if let Some(d)=digit{input.insert_char(d);}self.edit=Some(FieldEdit{target:t,input,prior_preset});self.focus=t;
    }
    fn restore_preset(&mut self,prior:Option<(usize,usize)>){if let Some((g,previous))=prior{self.presets[g]=previous;self.update_preset(g);self.dirty();}}
    fn cancel_field(&mut self){if let Some(edit)=self.edit.take(){self.restore_preset(edit.prior_preset);}}
    fn commit_field(&mut self){let Some(edit)=self.edit.take()else{return;};let t=edit.target;if t==Target::Template{if self.settings.template!=edit.input.text{self.settings.template=edit.input.text;self.dirty();}return;}let Ok(v)=edit.input.text.parse::<f64>()else{self.restore_preset(edit.prior_preset);return;};if !v.is_finite(){self.restore_preset(edit.prior_preset);return;}
        match t{Target::Custom=>{if let Some(g)=self.preset_group(){self.customs[g]=match g{0=>v.clamp(0.01,16.),1=>v.clamp(1.,1000.),_=>f64::from(pipe::snap8(v.clamp(8.,16384.)))};self.presets[g]=4;self.update_preset(g);}},Target::W=>self.settings.width=pipe::snap8(v.clamp(8.,16384.)),Target::H=>{self.settings.height=pipe::snap8(v.clamp(8.,16384.));self.settings.ratio=8;},Target::A|Target::B=>{let value=v.round().clamp(1.,16384.) as u32;if (t==Target::A)!=self.settings.portrait{self.settings.custom_ratio.0=value;}else{self.settings.custom_ratio.1=value;}},Target::Quality=>self.set_quality(v.round().clamp(1.,100.) as u8),_=>{}}self.dirty();}
    fn open_folder(&mut self,app:&AppState){let Some(source)=self.source()else{return;};let start=self.encoded.as_ref().and_then(|e|pipe::output_path(source,e,&self.settings.template).ok()).and_then(|p|p.parent().map(Path::to_path_buf)).unwrap_or_else(||source.path().parent().unwrap_or(Path::new(".")).to_path_buf());let start=start.ancestors().find(|p|p.is_dir()).unwrap_or(Path::new(".")).to_path_buf();self.folder=Some(FilePickerState::new(super::keybindings::file_picker_config_with_browse_sort(app,tui_file_picker::FilePickerConfig{start_dir:start,title:"Select image destination".into(),selection_mode:tui_file_picker::FilePickerSelectionMode::Directories,operation_policy:tui_file_picker::FileOperationPolicy::selection_only(app.file_task_verbose_degrade_notices),theme:super::keybindings::file_picker_theme_from_theme(&app.theme),..Default::default()})));}
    fn folder_action(&mut self,a:FilePickerAction){match a{FilePickerAction::Selected(p)=>{let name=self.settings.template.rsplit('/').next().unwrap_or("%NAME%.%EXT%");let dir=p.to_string_lossy().replace('\\',"/");self.settings.template=format!("{}/{name}",dir.trim_end_matches('/'));self.folder=None;self.dirty();},FilePickerAction::Cancelled=>self.folder=None,_=>{}}}
    fn popup_open(&mut self,p:Popup,focus:Target){if p==Popup::Help {self.help_return=self.popup.map(|old|(old,self.focus));}if self.popup.is_none(){self.popup_return=self.focus;}self.popup=Some(p);self.focus=focus;self.hover=None;}
    fn close_popup(&mut self){if self.popup==Some(Popup::Help){if let Some((old,focus))=self.help_return.take(){self.popup=Some(old);self.focus=focus;return;}}if self.popup==Some(Popup::Color){self.popup=self.color_return.take();self.focus=Target::Pad;}else{self.popup=None;self.focus=self.popup_return;}self.hover=None;}
    fn color_value(&self)->Option<[u8;4]>{let v=self.color_input.text.trim().trim_start_matches('#');let n=u32::from_str_radix(v,16).ok()?;match v.len(){6=>Some([(n>>16) as u8,(n>>8) as u8,n as u8,255]),8=>Some([(n>>24) as u8,(n>>16) as u8,(n>>8) as u8,n as u8]),_=>None}}
    fn effective_pad(&self)->[u8;4]{self.settings.pad_color(self.format()).0}
    fn set_channel(&mut self,c:usize,value:u8){let mut rgba=self.color_value().unwrap_or_else(||self.effective_pad());rgba[c]=value;self.color_input=TextInputState::new_selected(format!("{:02X}{:02X}{:02X}{:02X}",rgba[0],rgba[1],rgba[2],rgba[3]));}
    fn adjust_color(&mut self,c:usize,delta:i16){let v=self.color_value().unwrap_or_else(||self.effective_pad())[c];self.set_channel(c,(i16::from(v)+delta).clamp(0,255) as u8);}
    fn action(&mut self,t:Target,app:&AppState)->bool {match t {
        Target::Picker=>{if self.alternate.is_some(){self.popup_open(Popup::Source,Target::SourceChoice);}},Target::SourceChoice=>self.close_popup(),Target::AlternateChoice=>self.switch_source(),Target::Transform=>self.popup_open(Popup::Transform,Target::Rotate),
        Target::Mode(i)=>{self.settings.mode=MODES[i];self.dirty();},Target::Preset(i)=>{if let Some(g)=self.preset_group(){self.presets[g]=i;self.update_preset(g);self.dirty();}},
        Target::Custom|Target::W|Target::H|Target::A|Target::B|Target::Quality=>self.start_field(t,None),Target::Template=>{self.template_input=TextInputState::new_selected(self.settings.template.clone());self.popup_open(Popup::Template,Target::TemplateField);},Target::TemplateField=>{},Target::Token(i)=>{if let Some(token)=image_tokens(self.embedded).get(i){insert_image_token(&mut self.template_input,token);}},Target::Separator(i)=>{if let Some(sep)=IMAGE_SEPARATORS.get(i){self.template_input.insert_string(sep);}},Target::TemplateLeft=>self.template_input.cursor_left(),Target::TemplateRight=>self.template_input.cursor_right(),Target::TemplateBackspace=>self.template_input.backspace(),Target::TemplateClear=>self.template_input=TextInputState::empty(),Target::TemplateApply=>{if self.settings.template!=self.template_input.text{self.settings.template=self.template_input.text.clone();self.dirty();}self.close_popup();},Target::TemplateCancel=>self.close_popup(),Target::Orientation(p)=>{self.settings.portrait=p;self.dirty();},
        Target::Linear=>{self.settings.linear=!self.settings.linear;self.dirty();},Target::Upscale=>{self.settings.no_upscale=!self.settings.no_upscale;self.dirty();},Target::FlipH=>{self.settings.flip_h=!self.settings.flip_h;self.dirty();},Target::FlipV=>{self.settings.flip_v=!self.settings.flip_v;self.dirty();},
        Target::Pad=>{self.color_return=self.popup;let v=self.effective_pad();self.color_initial=v;self.color_input=TextInputState::new_selected(format!("{:02X}{:02X}{:02X}{:02X}",v[0],v[1],v[2],v[3]));self.popup=Some(Popup::Color);self.focus=Target::ColorField;},
        Target::ColorDefault=>{if self.settings.pad.take().is_some(){self.dirty();}self.close_popup();},Target::ColorDone=>{if let Some(rgba)=self.color_value(){if rgba!=self.color_initial{self.settings.pad=Some(rgba);self.dirty();}self.close_popup();}},Target::ColorField=>{self.color_input.select_all_text();},Target::ColorStep(channel,up)=>{self.adjust_color(channel,if up{1}else{-1});self.focus=Target::ColorChannel(channel);},Target::ColorChannel(_)=>{},
        Target::Browse=>self.open_folder(app),Target::Done=>self.close_popup(),Target::Help=>self.popup_open(Popup::Help,Target::Done),Target::Ok=>{if !summary(self).3&&!self.sources.is_empty(){self.request=Some(Request::Ok);}},Target::Preview=>{if self.problem.is_none()&&!self.sources.is_empty(){self.request=Some(Request::Preview);}},Target::Cancel=>return false,
        _ if Self::is_cycler(t)=>self.cycle(t,1),_=>{} }true }
    pub(super) fn prepare(&mut self,picker:&mut ratatui_image::picker::Picker,generation:usize,retransmit:usize,theme:Theme)->bool{
        let mut changed=false;
        if let Some(job)=self.job.as_mut(){loop{match job.events.poll(){Ok(JobEvent::Begin(index,update))=>{job.displayed=Some(index);job.progress.apply_update(update);changed=true;},Ok(JobEvent::Update(update))=>{job.progress.apply_update(update);changed=true;},Ok(JobEvent::Done(result,totals,effects))=>{changed=true;job.displayed=None;self.wrote|=effects.changed();self.cleanup_pending|=effects.cleanup_pending!=0;match result{Ok(status)=>{if self.embedded||self.sources.len()==1{self.finished=Some(status);}else{job.progress.apply_update(FileTaskProgressUpdate::Finished{status,totals});}},Err(failure)=>{let status=format!("{}; {} completed, {} incomplete, {} skipped, {} unattempted",failure.status,totals.completed,totals.errors,totals.skipped,totals.not_attempted);if failure.aborted{job.progress.apply_update(FileTaskProgressUpdate::Aborted{status,totals});}else{job.progress.apply_update(FileTaskProgressUpdate::Failed{status,totals});}}}break;},Err(mpsc::TryRecvError::Empty)=>break,Err(mpsc::TryRecvError::Disconnected)=>{if !job.progress.is_terminal(){job.progress.apply_update(FileTaskProgressUpdate::Failed{status:"Image worker exited".into(),totals:job.progress.totals});self.cleanup_pending=true;changed=true;}break;}}}return changed;}
        if let Some(preview)=self.preview.as_mut(){return preview.prepare(picker,generation,retransmit,theme.bg);}
        if let Some(load)=self.load.as_ref(){match load.poll(){Ok(Ok(loaded))=>{self.sources=loaded.primary;self.alternate=loaded.alternate;self.index=loaded.index;self.decoded=loaded.focused.map(|d|(self.sources[self.index].clone(),d));self.load=None;self.changed=Instant::now()-Duration::from_millis(150);changed=true;},Ok(Err(e))=>{self.problem=Some(e);self.load=None;changed=true;},Err(mpsc::TryRecvError::Disconnected)=>{self.problem=Some("Image discovery worker exited".into());self.load=None;changed=true;},Err(mpsc::TryRecvError::Empty)=>{}}}
        if let Some(work)=self.work.as_ref(){let revision=work.revision;match work.pending.poll(){Ok(w)=>{self.work=None;let current=Some(&w.source)==self.source();if current{match w.result{Ok((d,e))=>{self.decoded=Some((w.source,d));if w.revision==self.revision{match e{Ok(e)=>self.encoded=Some(e),Err(e)=>self.problem=Some(e)}}},Err(e) if w.revision==self.revision=>self.problem=Some(e),Err(_)=>{}}}changed=true;},Err(mpsc::TryRecvError::Disconnected)=>{self.work=None;if revision==self.revision{self.problem=Some("Image encode worker exited".into());}changed=true;},Err(mpsc::TryRecvError::Empty)=>{}}}
        if self.work.is_none()&&self.encoded.is_none()&&self.problem.is_none()&&self.changed.elapsed()>=Duration::from_millis(150){if let Some(source)=self.source().cloned(){let cached=self.current().cloned();let revision=self.revision;let settings=self.settings.clone();self.work=Some(EncodeWorker::spawn(source,cached,settings,revision));changed=true;}}
        if let(Some(request),Some(encoded),Some(decoded))=(self.request,self.encoded.clone(),self.current().cloned()){
            self.request=None;
            if matches!(request,Request::Ok) && summary(self).3 { return true; }
            match request{Request::Preview=>self.preview=Some(Preview::new(decoded.pixels.clone(),&encoded,theme.bg)),Request::Ok=>{let (sources,index)=if self.embedded{(vec![self.sources[self.index].clone()],0)}else{(self.sources.clone(),self.index)};self.job=Some(Job::start(sources,index,decoded,encoded,self.settings.clone(),theme));}};changed=true;
        }changed
    }
    pub(super) fn cancel_running(&mut self)->bool { if let Some(job)=self.job.as_mut(){if !job.progress.is_terminal(){job.action(FileTaskUserAction::Abort);return true;}}false }
    pub(super) fn kitty_upload_withheld(&mut self){if let Some(p)=self.preview.as_mut(){p.viewer.lock().unwrap_or_else(|e|e.into_inner()).kitty_upload_withheld();}}
    fn move_focus(&mut self,dx:i32,dy:i32,band:bool){
        // The quality bar is mouse-only; the arrow-to-arrow span is the
        // stepper's sole keyboard stop, including its editable numeric well.
        let hits=||self.hits.iter().filter(|h|h.target!=Target::QualityBar);
        let Some(current)=hits().find(|h|h.target==self.focus).or_else(||hits().next()).copied() else {return;};
        let next=if band&&self.popup.is_none(){
            let b=step(current.band.min(2) as usize,3,dy) as u8;
            hits().find(|h|h.band==b)
        }else if dy!=0{
            let y=hits().filter(|h|if dy>0{h.y>current.y}else{h.y<current.y}).map(|h|h.y).reduce(|a,b|if dy>0{a.min(b)}else{a.max(b)});
            y.and_then(|y|hits().filter(|h|h.y==y).min_by_key(|h|h.x.abs_diff(current.x)))
        }else{
            hits().filter(|h|h.y==current.y&&if dx>0{h.x>current.x}else{h.x<current.x}).min_by_key(|h|h.x.abs_diff(current.x))
        };
        if let Some(n)=next{self.focus=n.target;}
    }
    fn key(&mut self,app:&AppState,key:KeyEvent)->bool{
        if let Some(job)=self.job.as_mut(){if key.code==KeyCode::Char('q'){return true;}let a=job.progress.handle_key(key);if !job.action(a){self.finished=Some(job.progress.status.clone());}return true;}
        if let Some(folder)=self.folder.as_mut(){let a=folder.handle_key(key);self.folder_action(a);return true;}
        if let Some(p)=self.preview.as_mut(){if p.show_help {if matches!(key.code,KeyCode::Esc|KeyCode::Enter){p.show_help=false;p.display(app.theme.bg);}return true;}match key.code{KeyCode::Esc=>{self.preview=None;},KeyCode::Char(' ')|KeyCode::Left|KeyCode::Right=>p.toggle(!p.show_after,app.theme.bg),KeyCode::Char('?')=>{p.show_help=true;},_=>{}}return true;}
        let alt_ok=key.code==KeyCode::Char('o')&&key.modifiers.contains(KeyModifiers::ALT);
        if self.edit.is_some(){match key.code{KeyCode::Esc=>{self.cancel_field();return true;},KeyCode::Enter=>{self.commit_field();return true;},KeyCode::Tab if self.edit.as_ref().is_some_and(|e|e.target==Target::Template)=>{if let Some(e)=self.edit.as_mut(){let _=complete_template(&mut e.input,self.embedded);}return true;},KeyCode::Tab|KeyCode::BackTab=>{self.commit_field();self.move_focus(0,if key.code==KeyCode::BackTab{-1}else{1},true);return true;},_ if alt_ok=>{let valid=self.edit.as_ref().is_some_and(|e|e.target==Target::Template||e.input.text.parse::<f64>().is_ok_and(f64::is_finite));self.commit_field();if valid{return self.action(Target::Ok,app);}return true;},_=>{if let Some(e)=self.edit.as_mut(){if e.target==Target::Template||!matches!(key.code,KeyCode::Char(c) if !c.is_ascii_digit()&&c!='.'&&!key.modifiers.intersects(KeyModifiers::ALT|KeyModifiers::CONTROL)){handle_text_input_key(&mut e.input,&key);}}return true;}}}
        if self.popup==Some(Popup::Color)&&self.focus==Target::ColorField{match key.code{KeyCode::Esc=>self.close_popup(),KeyCode::Enter=>{self.action(Target::ColorDone,app);},KeyCode::Tab|KeyCode::Down=>self.focus=Target::ColorChannel(0),_=>{handle_text_input_key(&mut self.color_input,&key);}}return true;}
        if self.popup==Some(Popup::Color){if let Target::ColorChannel(c)=self.focus{match key.code{KeyCode::Left|KeyCode::Char('[')=>{self.adjust_color(c,-1);return true;},KeyCode::Right|KeyCode::Char(']')=>{self.adjust_color(c,1);return true;},_=>{}}}}
        if self.popup==Some(Popup::Template)&&self.focus==Target::TemplateField{match key.code{KeyCode::Esc=>self.close_popup(),KeyCode::Enter=>{self.action(Target::TemplateApply,app);},KeyCode::Tab=>{if let Some(i)=complete_template(&mut self.template_input,self.embedded){self.focus=Target::Token(i);}},KeyCode::Down=>self.focus=Target::Token(0),KeyCode::BackTab=>self.focus=Target::TemplateApply,_=>{handle_text_input_key(&mut self.template_input,&key);}}return true;}
        if self.popup.is_some()&&matches!(key.code,KeyCode::Tab|KeyCode::BackTab){let mut order=Vec::new();for h in &self.hits{if !order.contains(&h.target){order.push(h.target);}}if let Some(i)=order.iter().position(|t|*t==self.focus){self.focus=order[step(i,order.len(),if key.code==KeyCode::Tab{1}else{-1})];}return true;}
        if key.code==KeyCode::Esc{if self.popup.is_some(){self.close_popup();return true;}return false;}
        if self.popup==Some(Popup::Help){if key.code==KeyCode::Enter{self.close_popup();}return true;}
        if self.popup.is_none()&&alt_ok{return self.action(Target::Ok,app);}
        match key.code{
            KeyCode::Up=>self.move_focus(0,-1,false),KeyCode::Down=>self.move_focus(0,1,false),KeyCode::Left=>self.move_focus(-1,0,false),KeyCode::Right=>self.move_focus(1,0,false),KeyCode::Tab=>self.move_focus(0,1,true),KeyCode::BackTab=>self.move_focus(0,-1,true),
            KeyCode::Char('[')|KeyCode::Char(']')=>{let d=if key.code==KeyCode::Char('['){-1}else{1};if Self::is_cycler(self.focus){self.cycle(self.focus,d);}else if self.popup.is_none(){self.cycle_source(d);}},
            KeyCode::Char(' ')|KeyCode::Enter=>return self.action(self.focus,app),KeyCode::Char('p') if self.popup.is_none()&&key.modifiers==KeyModifiers::NONE=>return self.action(Target::Preview,app),
            KeyCode::Char('?')=>{self.popup_open(Popup::Help,Target::Done);},KeyCode::Char(c) if c.is_ascii_digit()&&key.modifiers==KeyModifiers::NONE=>{let t=if matches!(self.focus,Target::Preset(_)){Target::Custom}else{self.focus};if Self::number(t){self.start_field(t,Some(c));}},_=>{} }true
    }
    fn mouse(&mut self,app:&AppState,m:MouseEvent)->bool{
        if let Some(job)=self.job.as_mut(){let a=job.progress.handle_mouse(m,Rect::default());if !job.action(a){self.finished=Some(job.progress.status.clone());}return true;}
        if let Some(folder)=self.folder.as_mut(){let a=folder.handle_mouse(m,Rect::default());self.folder_action(a);return true;}
        if let Some(p)=self.preview.as_mut(){if matches!(m.kind,MouseEventKind::Down(MouseButton::Left)){let screen=self.popup_area;if p.show_help{p.show_help=false;p.display(app.theme.bg);return true;}if m.row==screen.bottom().saturating_sub(1){let x=screen.x+screen.width.saturating_sub(22)/2;if m.column>=x&&m.column<x+19{p.toggle(m.column<x+10,app.theme.bg);}else if m.column>=screen.right().saturating_sub(10){p.show_help=true;}}}return true;}
        let area=if self.popup.is_some(){self.popup_area}else{self.area};let inside=m.column>=area.x&&m.column<area.right()&&m.row>=area.y&&m.row<area.bottom();let hit=if inside{self.hits.iter().rev().find(|h|h.contains(m.column-area.x,m.row-area.y)).copied()}else{None};
        if m.kind==MouseEventKind::Moved{self.hover=hit.map(|h|if h.target==Target::QualityBar{Target::Quality}else{h.target});return true;}
        if !matches!(m.kind,MouseEventKind::Down(MouseButton::Left)){return true;}
        if let Some(edit)=self.edit.as_ref(){if hit.is_some_and(|h|h.target==edit.target&&(h.target!=Target::Quality||(53..=57).contains(&(m.column-area.x)))){return true;}self.commit_field();}
        if !inside&&self.popup.is_some(){self.close_popup();return true;}
        if let Some(h)=hit{self.focus=h.target;let x=m.column-area.x;if let Target::ColorChannel(c)=h.target{self.set_channel(c,(((x-h.x) as u32*255)/u32::from(h.w.saturating_sub(1).max(1))) as u8);return true;}if h.target==Target::TemplateField{self.template_input.place_cursor_from_view_column(54,usize::from(x.saturating_sub(h.x+1)));return true;}if h.target==Target::Quality{if (53..=57).contains(&x){return self.action(Target::Quality,app);}self.cycle(Target::Quality,if x<53{-1}else{1});return true;}if Self::is_cycler(h.target){self.cycle(h.target,if x==h.x{-1}else{1});return true;}if h.target==Target::QualityBar{self.set_quality(((x-h.x+1)*10).min(100) as u8);self.focus=Target::Quality;self.dirty();return true;}return self.action(h.target,app);}true
    }
}
fn step(i:usize,n:usize,d:i32)->usize{(i as i32+d).rem_euclid(n as i32) as usize}
fn cycle_value<T:Copy+PartialEq>(all:&[T],value:T,d:i32)->T{all[step(all.iter().position(|v|*v==value).unwrap_or(0),all.len(),d)]}
fn label<T:Copy+PartialEq>(all:&[T],labels:&[&'static str],value:T)->&'static str{labels[all.iter().position(|v|*v==value).unwrap_or(0)]}
const IMAGE_SEPARATORS:[&str;5]=["/","_","-","."," "];
fn image_tokens(embedded:bool)->&'static [&'static str]{if embedded{&["DIR","NAME","EXT","W","H","ALBUMDIR","ALBUM","ARTIST","PICTYPE"]}else{&["DIR","NAME","EXT","W","H"]}}
fn partial_token(input:&TextInputState)->Option<(usize,usize,String)>{let cursor=input.cursor;let prefix=input.text.get(..cursor)?;if prefix.bytes().filter(|b|*b==b'%').count()%2==0{return None;}let start=prefix.rfind('%')?;let end=if input.text[cursor..].starts_with('%'){cursor+1}else{cursor};Some((start,end,prefix[start+1..].to_ascii_uppercase()))}
fn insert_image_token(input:&mut TextInputState,token:&str){if let Some((start,end,_))=partial_token(input){input.replace_range(start..end,&format!("%{token}%"));}else{super::template_builder::insert_token(input,token);}}
fn complete_template(input:&mut TextInputState,embedded:bool)->Option<usize>{let (_,_,partial)=partial_token(input)?;let mut matches=image_tokens(embedded).iter().enumerate().filter(|(_,t)|t.starts_with(&partial));let(i,token)=matches.next()?;if matches.next().is_none(){insert_image_token(input,token);None}else{Some(i)}}

pub(super) fn handle_key(app:&mut AppState,state:&mut EditImageState,key:KeyEvent)->bool{let preview=state.preview.is_some();let keep=state.key(app,key);if preview&&state.preview.is_none(){app.force_redraw=true;}keep}
pub(super) fn handle_mouse(app:&mut AppState,state:&mut EditImageState,m:MouseEvent)->bool{state.mouse(app,m)}

// Renderer and hit-map construction share the same coordinate arguments. A
// hidden control produces neither cells nor a hit region. Geometry is fixed
// independently of the current values and the presence of optional controls.
struct Form<'a,'b> { f:&'a mut Frame<'b>, state:&'a mut EditImageState, theme:Theme, area:Rect, band:u8, bg:Color }
impl Form<'_,'_> {
    fn text(&mut self,x:u16,y:u16,s:&str,color:Color){ui::paint(self.f,self.area,x,y,&dw::truncate_right(s,usize::from(self.area.width.saturating_sub(3).saturating_sub(x))),color,self.bg);}
    fn label(&mut self,x:u16,y:u16,s:&str){self.text(x,y,s,self.theme.label);}
    fn soft(&mut self,x:u16,y:u16,s:&str){self.text(x,y,s,super::theme::mix_colors(self.theme.label,self.theme.value,1,2));}
    fn hit(&mut self,t:Target,x:u16,y:u16,w:u16)->Color {self.state.hits.push(Hit::new(t,x,y,w,self.band));let bg=if self.state.focus==t&&(self.state.edit.is_none()||t==Target::Quality){self.theme.selection_bg}else if self.state.hover==Some(t){self.theme.hover_bg}else{self.bg};ui::fill(self.f,Rect::new(self.area.x+x,self.area.y+y,w,1),bg);bg}
    fn band(&mut self,b:u8,y:u16,h:u16,title:&str,focused:u8){self.band=b;self.bg=ui::band_color(self.theme,b==focused);ui::fill(self.f,Rect::new(self.area.x+1,self.area.y+y,self.area.width-2,h),self.bg);self.text(3,y,title,if b==focused{self.theme.header}else{self.theme.label});}
    fn radio(&mut self,t:Target,x:u16,y:u16,name:&str,on:bool){let w=2+dw::width(name) as u16;let bg=self.hit(t,x,y,w);ui::paint(self.f,self.area,x,y,if on{"●"}else{"○"},if on{self.theme.info}else{self.theme.label},bg);ui::paint(self.f,self.area,x+2,y,name,self.theme.value,bg);}
    fn toggle(&mut self,t:Target,x:u16,y:u16,name:&str,on:bool){
        let w=4+dw::width(name) as u16;let bg=self.hit(t,x,y,w);
        // Round 1 owner's checkbox glyphs override the bracketed mockup.
        // Center the one-cell glyph in the original three-cell well so
        // labels, hit areas and popup overlap retain their mockup columns.
        ui::paint(self.f,self.area,x+1,y,if on{"☑"}else{"☐"},if on{self.theme.info}else{self.theme.label},bg);
        ui::paint(self.f,self.area,x+4,y,name,self.theme.value,bg);
    }
    fn cycler(&mut self,t:Target,x:u16,y:u16,w:u16,value:&str){let bg=self.hit(t,x,y,w+4);ui::paint(self.f,self.area,x,y,"◂",self.theme.info,bg);ui::paint(self.f,self.area,x+2,y,&dw::pad_or_truncate(value,w as usize,false),self.theme.value,bg);ui::paint(self.f,self.area,x+w+3,y,"▸",self.theme.info,bg);}
    fn field(&mut self,t:Target,x:u16,y:u16,w:u16,value:&str,readonly:bool){self.state.hits.push(Hit::new(t,x,y,w,self.band));self.field_well(t,x,y,w,value,readonly,true);}
    fn field_well(&mut self,t:Target,x:u16,y:u16,w:u16,value:&str,readonly:bool,focus_marker:bool){let bg=ui::well(self.theme);ui::fill(self.f,Rect::new(self.area.x+x,self.area.y+y,w,1),bg);let edit=self.state.edit.as_ref().filter(|e|e.target==t);let (digits,caret)=if let Some(e)=edit{e.input.view(w as usize)}else{(dw::truncate_left(value,w as usize),0)};let used=dw::width(&digits) as u16;let start=x+w.saturating_sub(used+u16::from(used<w));ui::paint(self.f,self.area,start,y,&digits,if readonly&&edit.is_none(){super::theme::mix_colors(self.theme.label,self.theme.value,1,2)}else{self.theme.value},bg);if edit.is_some(){self.f.buffer_mut().get_mut(self.area.x+(start+caret).min(x+w-1),self.area.y+y).set_bg(self.theme.editing_cursor);}else if focus_marker&&self.state.focus==t{self.f.buffer_mut().get_mut(self.area.x+x,self.area.y+y).set_bg(self.theme.selection_bg);}}

    fn button(&mut self,t:Target,x:u16,y:u16,w:u16,text:&str,bg:Color,chip:bool){self.state.hits.push(Hit::new(t,x,y,w,self.band));let focus=self.state.focus==t;let hovered=self.state.hover==Some(t);let ground=if focus{self.theme.pill_active_bg}else if hovered&&!chip{self.theme.pill_neutral_hover_bg}else{bg};let fg=if focus{self.theme.pill_active_fg}else if chip{self.theme.panel_bg}else{self.theme.value};ui::fill(self.f,Rect::new(self.area.x+x,self.area.y+y,w,1),ground);ui::paint(self.f,self.area,x,y,&dw::pad_or_truncate(text,w as usize,false),fg,ground);}
    fn swatch(&mut self,x:u16,y:u16,w:u16,rgba:[u8;4],bg:Color){
        let color=Color::Rgb(rgba[0],rgba[1],rgba[2]);
        let light=super::theme::mix_colors(bg,self.theme.value,1,2);
        for i in 0..w{let under=if i%2==0{bg}else{light};let shown=super::theme::mix_colors(under,color,u16::from(rgba[3]),255);ui::paint_raw(self.f,self.area,x+i,y,"█",shown,bg);}
    }
    fn pad(&mut self,x:u16,y:u16){let bg=self.hit(Target::Pad,x,y,4);let rgba=self.state.effective_pad();ui::paint(self.f,self.area,x,y,"▕",self.theme.label,bg);self.swatch(x+1,y,2,rgba,bg);ui::paint(self.f,self.area,x+3,y,"▏",self.theme.label,bg);}
    fn help(&mut self,x:u16,y:u16){let bg=self.hit(Target::Help,x,y,10);ui::paint(self.f,self.area,x,y,"?",self.theme.info,bg);ui::paint(self.f,self.area,x+2,y,"all keys",self.theme.label,bg);}
}
fn centered(screen:Rect,w:u16,h:u16)->Rect{if screen.width<w||screen.height<h{return Rect::default();}Rect::new(screen.x+(screen.width-w)/2,screen.y+(screen.height-h)/2,w,h)}
fn dim(f:&mut Frame,area:Rect,theme:Theme){let buffer=f.buffer_mut();for y in area.y..area.bottom(){for x in area.x..area.right(){let cell=buffer.get_mut(x,y);cell.set_fg(theme.text_dim);}}}
fn dim_editor_popup(f:&mut Frame,area:Rect,theme:Theme){
    let buffer=f.buffer_mut();
    for y in area.y..area.bottom(){for x in area.x..area.right(){
        let cell=buffer.get_mut(x,y);cell.set_fg(theme.text_dim);
        cell.set_bg(super::theme::mix_colors(theme.panel_bg,cell.bg,1,3));
    }}
}
fn band_for(t:Target)->u8{match t{Target::Picker|Target::Image|Target::Transform=>0,Target::Mode(_)|Target::Preset(_)|Target::Custom|Target::W|Target::H|Target::Ratio|Target::A|Target::B|Target::Orientation(_)|Target::Fill|Target::Pad|Target::Filter|Target::Linear|Target::Upscale=>1,Target::Format|Target::Quality|Target::QualityBar|Target::Metadata|Target::Conflict|Target::Write|Target::Template|Target::Browse=>2,_=>3}}
fn transform_label(s:&Settings)->String{let mut v=Vec::new();if s.rotation>0{v.push(format!("Rotate {}",ROTATIONS[s.rotation]));}if s.flip_h{v.push("Flip horizontal".into());}if s.flip_v{v.push("Flip vertical".into());}if s.square!=Square::Off{v.push(format!("Square, {}",SQUARE_NAMES[SQUARES.iter().position(|v|*v==s.square).unwrap_or(0)].to_lowercase()));}if v.is_empty(){"none".into()}else{v.join(", ")}}
fn size_label(size:usize)->String{let (n,unit)=if size>=1024*1024{(size as f64/(1024.*1024.),"MiB")}else if size>=1024{(size as f64/1024.,"KiB")}else{return format!("{size} B");};if n<100.{format!("{n:.1} {unit}")}else{format!("{n:.0} {unit}")}}
fn safe_label(s:&str)->String{s.chars().map(|c|if c.is_control(){'�'}else{c}).collect()}
fn source_title(s:&EditImageState)->String{if s.embedded{format!("Embedded artwork · {}",s.clicked.file_name().unwrap_or_default().to_string_lossy())}else if s.sources.len()>1{format!("{} images",s.sources.len())}else{s.clicked.file_name().unwrap_or_default().to_string_lossy().into_owned()}}
fn home_display(s:&str)->String{if let Some(home)=std::env::var_os("HOME").or_else(||std::env::var_os("USERPROFILE")){let home=home.to_string_lossy().replace('\\',"/");if let Some(rest)=s.strip_prefix(&home){if rest.starts_with('/'){return format!("~{rest}");}}}s.to_string()}
fn template_display(s:&str,w:usize)->String{let s=home_display(s);if dw::width(&s)<=w{return s;}for (i,_) in s.match_indices('/'){let tail=format!("…{}",&s[i..]);if dw::width(&tail)<=w{return tail;}}dw::truncate_middle(&s,w)}
fn summary(state:&EditImageState)->(String,String,String,bool){
    if let Some(e)=&state.problem{return (String::new(),safe_label(e),String::new(),true);}
    if state.settings.write!=WriteMode::File&&state.source().is_some_and(|s|!s.can_reembed()){return(String::new(),"This embedded tag is read-only; choose File".into(),String::new(),true);}
    if state.files_visible(){if let (Some(src),Some(encoded))=(state.source(),state.encoded.as_ref()){
        if let Err(e)=pipe::output_path(src,encoded,&state.settings.template){return(String::new(),safe_label(&e),String::new(),true);}
    }}
    let Some(d)=state.current()else{return (String::new(),"Loading image…".into(),String::new(),false);};
    let g=state.geometry().unwrap();let input=format!("{}×{} {} {}",d.pixels.width(),d.pixels.height(),d.format.short(),size_label(d.bytes));
    let output=format!("{}×{} {} {}",g.target.0,g.target.1,state.format().short(),state.encoded.as_ref().map(|e|size_label(e.bytes.len())).unwrap_or_else(||"…".into()));
    let mut tail:String=if state.embedded {match state.settings.write{WriteMode::File=>"1 file".into(),WriteMode::Embed=>"1 track".into(),WriteMode::Both=>"1 file + 1 track".into()}}else if state.sources.len()==1{"1 file".into()}else{format!("{} files",state.sources.len())};if g.kept{tail.push_str(" · size kept");}(input,output,tail,false)
}
fn draw_form(f:&mut Frame,s:&mut EditImageState,theme:Theme){
    let screen=f.size();s.area=if screen.width>=80&&screen.height>=s.height()+2{centered(screen,WIDTH,s.height())}else{Rect::default()};s.hits.clear();let area=s.area;
    if area.width==0 {let message=format!("Edit Image requires an 80 × {} terminal",s.height()+2);ui::paint(f,screen,screen.width.saturating_sub(dw::width(&message) as u16)/2,screen.height/2,&message,theme.value,theme.bg);return;}
    let extra=if s.embedded{1}else{0};ui::fixed_frame(f,area,"edit image",&[4,11,16+extra,18+extra],theme);
    let settings=s.settings.clone();let focus=if s.popup.is_some(){s.popup_return}else{s.focus};let focused=band_for(focus);
    let geom=s.geometry();let requested_geom=s.requested_geometry();
    let source_name=safe_label(&source_title(s));let title=if s.alternate.is_some(){format!("[▾ {} ◆]",dw::truncate_middle(&source_name,51))}else{dw::truncate_middle(&source_name,57)};let source_label=s.source().map(|p|safe_label(&p.label())).unwrap_or_else(||"Loading…".into());let count=s.sources.len();let idx=s.index;
    let format=s.format();let template=template_display(&settings.template,43);let (input,output,tail,blocked)=summary(s);
    let mut v=Form{f,state:s,theme,area,band:0,bg:theme.panel_bg};
    if v.state.alternate.is_some(){v.state.hits.push(Hit::new(Target::Picker,15,0,dw::width(&title) as u16,0));}ui::paint(v.f,area,15,0,&title,theme.panel_bg,theme.title);
    v.band(0,1,3,"SOURCE",focused);v.label(3,2,if v.state.embedded{"Picture"}else{"Image"});if count>1{v.cycler(Target::Image,16,2,22,&dw::truncate_middle(&source_label,22));v.soft(44,2,&format!("{} of {count}",idx+1));}else{v.text(16,2,&dw::truncate_middle(&source_label,56),theme.value);}
    v.button(Target::Transform,3,3,12," Transform… ",theme.pill_neutral_bg,false);let transforms=transform_label(&settings);if transforms=="none"{v.soft(16,3,&transforms);}else{v.text(16,3,&transforms,theme.value);}
    v.band(1,5,6,"RESIZE",focused);v.label(3,6,"Mode");for (i,x) in [16,23,32,43,56].into_iter().enumerate(){v.radio(Target::Mode(i),x,6,MODE_NAMES[i],settings.mode==MODES[i]);}
    if let Some(group)=v.state.preset_group(){v.label(3,7,MODE_NAMES[group+1]);let mut x=16;for(i,name)in PRESET_NAMES[group].iter().enumerate(){v.radio(Target::Preset(i),x,7,name,v.state.presets[group]==i);x+=4+dw::width(name) as u16;}v.radio(Target::Preset(4),x,7,"Custom",v.state.presets[group]==4);let text=if v.state.presets[group]==4{v.state.customs[group].to_string()}else{String::new()};v.field(Target::Custom,x+9,7,6,&text,false);v.soft(x+16,7,["×","%","px"][group]);}
    if settings.mode==Mode::Size{
        v.label(3,7,"Size");v.label(16,7,"W");v.field(Target::W,18,7,6,&settings.width.to_string(),false);v.soft(25,7,"×");v.label(27,7,"H");let h=if settings.ratio==8{settings.height}else{requested_geom.map(|g|g.target.1).unwrap_or(settings.height)};v.field(Target::H,29,7,6,&h.to_string(),settings.ratio!=8);
        v.label(3,8,"Ratio");let ratio=if settings.ratio==0{"Source".into()}else if settings.ratio==8{format!("Free · {:.2}:1",f64::from(settings.width)/f64::from(settings.height.max(1)))}else if settings.ratio==7{String::new()}else{let(a,b)=settings.ratio_terms().unwrap();format!("{a}:{b}")};
        v.cycler(Target::Ratio,16,8,13,&ratio);
        if settings.ratio==7 { // narrower field hits take precedence over the cycler's value hit
            let (a,b)=settings.ratio_terms().unwrap_or(settings.custom_ratio);v.field(Target::A,18,8,5,&a.to_string(),false);v.soft(24,8,":");v.field(Target::B,26,8,5,&b.to_string(),false);
        }
        if settings.ratio_terms().is_some(){v.radio(Target::Orientation(false),40,8,"Landscape",!settings.portrait);v.radio(Target::Orientation(true),53,8,"Portrait",settings.portrait);}
        if let Some(g)=requested_geom {let (sw,sh)=g.transformed;let (w,h)=g.target;let differs=(f64::from(w)*f64::from(sh)/(f64::from(h)*f64::from(sw))-1.).abs()>0.005;if settings.ratio!=0&&differs{v.label(3,9,"Fill");v.cycler(Target::Fill,16,9,12,label(&FILLS,&FILL_NAMES,settings.fill));if settings.fill==Fill::Pad{v.pad(34,9);}v.soft(40,9,&g.note());}}
    }else{v.label(3,8,"Size");v.soft(16,8,&geom.map(|g|format!("{} × {}",g.target.0,g.target.1)).unwrap_or_else(||"…".into()));}
    if settings.mode!=Mode::Off{v.label(3,10,"Filter");v.cycler(Target::Filter,16,10,10,FILTERS[settings.filter]);v.toggle(Target::Linear,40,10,"Linear light",settings.linear);if settings.mode!=Mode::Scale{v.toggle(Target::Upscale,58,10,"No upscale",settings.no_upscale);}}
    v.band(2,12,4+extra,"OUTPUT",focused);v.label(3,13,"Format");v.cycler(Target::Format,16,13,15,settings.format.label());
    if format.has_quality(){let quality=settings.quality_for(format);v.label(40,13,"Quality");let bg=v.hit(Target::Quality,51,13,9);ui::paint(v.f,area,51,13,"◂",theme.info,bg);ui::paint(v.f,area,59,13,"▸",theme.info,bg);v.field_well(Target::Quality,53,13,5,&quality.to_string(),false,false);v.state.hits.push(Hit::new(Target::QualityBar,61,13,10,2));let n=(u16::from(quality)+5)/10;ui::paint_raw(v.f,area,61,13,&"█".repeat(n as usize),theme.info,v.bg);ui::paint_raw(v.f,area,61+n,13,&"░".repeat((10-n) as usize),theme.text_dim,v.bg);}
    v.label(3,14,"Metadata");v.cycler(Target::Metadata,16,14,13,label(&META,&META_NAMES,settings.metadata));if v.state.files_visible(){v.label(40,14,"If exists");v.cycler(Target::Conflict,51,14,12,label(&CONFLICTS,&CONFLICT_NAMES,settings.conflict));}
    if v.state.embedded{v.label(3,15,"Write");v.cycler(Target::Write,16,15,15,label(&WRITES,&WRITE_NAMES,settings.write));}
    if v.state.files_visible(){let y=15+extra;v.label(3,y,"Save to");v.state.hits.push(Hit::new(Target::Template,16,y,45,2));let bg=ui::well(theme);ui::fill(v.f,Rect::new(area.x+16,area.y+y,45,1),bg);let edit=v.state.edit.as_ref().filter(|e|e.target==Target::Template);let text=edit.map(|e|e.input.view(43).0).unwrap_or(template);let mut x=17;
        // Paint a literal run, then the complete token including both delimiters.
        // Splitting *after* '%' otherwise gives an opening delimiter to the
        // preceding literal run. An unfinished token stays purple while typing.
        let mut rest=text.as_str();
        while let Some(start)=rest.find('%') {
            let (literal,opening)=rest.split_at(start);
            ui::paint(v.f,area,x,y,literal,theme.value,bg);
            x+=dw::width(literal) as u16;
            let end=opening[1..].find('%').map_or(opening.len(),|i|i+2);
            let (token,tail)=opening.split_at(end);
            ui::paint(v.f,area,x,y,token,theme.purple,bg);
            x+=dw::width(token) as u16;
            rest=tail;
        }
        ui::paint(v.f,area,x,y,rest,theme.value,bg);if let Some(e)=edit{let (_,cursor)=e.input.view(43);ui::fill(v.f,Rect::new(area.x+17+cursor.min(43),area.y+y,1,1),theme.editing_cursor);}v.button(Target::Browse,64,y,9," Browse… ",theme.pill_neutral_bg,false);}
    let y=17+extra;let mut whole=if input.is_empty(){output.clone()}else{format!("{input}  →  {output} · {tail}")};whole=dw::truncate_right(&whole,70);let x=(WIDTH-dw::width(&whole) as u16)/2;v.bg=theme.panel_bg;v.band=3;
    if input.is_empty()||dw::width(&format!("{input}  →  {output} · {tail}"))>70{v.text(x,y,&whole,if blocked{theme.error}else{theme.value});}else{v.soft(x,y,&input);let x=x+dw::width(&input) as u16;v.text(x+2,y,"→",theme.info);v.text(x+5,y,&output,theme.value);let x=x+5+dw::width(&output) as u16;ui::paint_raw(v.f,area,x,y," · ",theme.text_dim,theme.panel_bg);v.soft(x+3,y,&tail);}
    let y=19+extra;if !blocked&&!v.state.sources.is_empty(){v.button(Target::Ok,22,y,4," OK ",theme.chip_go,true);v.button(Target::Preview,28,y,11," p Preview ",theme.tab_active,true);}v.button(Target::Cancel,41,y,12," Esc Cancel ",theme.chip_dismiss,true);v.help(63,y);
    if v.state.popup.is_none()&&!v.state.hits.iter().any(|h|h.target==v.state.focus){if let Some(h)=v.state.hits.iter().find(|h|h.band==focused).or(v.state.hits.first()){v.state.focus=h.target;}}
}
fn draw_popup(f:&mut Frame,s:&mut EditImageState,theme:Theme){let Some(popup)=s.popup else{return;};dim_editor_popup(f,s.area,theme);s.hits.clear();let screen=f.size();let(w,h,title,rules) = match popup {Popup::Transform=>(46,11,"Transform",vec![8]),Popup::Source=>(52,6,"Source",vec![]),Popup::Color=>(48,12,"Pad color",vec![9]),Popup::Template=>(64,12,"Save to — template",vec![3,9]),Popup::Help=>(60,17,"All keys",vec![14])};let mut area=centered(screen,w,h);if popup==Popup::Transform&&area.width>0{area.y=(area.y+2).min(screen.bottom()-area.height);}s.popup_area=area;if area.width==0{return;}
    ui::fixed_frame(f,area,title,&rules,theme);for y in 1..area.height-1{if !rules.contains(&y){ui::fill(f,Rect::new(area.x+1,area.y+y,area.width-2,1),theme.dropdown_bg);}}
    let settings=s.settings.clone();let mut v=Form{f,state:s,theme,area,band:0,bg:theme.dropdown_bg};match popup{
        Popup::Transform=>{ui::fill(v.f,Rect::new(area.x+1,area.y+1,area.width-2,1),theme.surface);v.bg=theme.surface;v.soft(3,1,"Applied before resizing, in this order.");v.bg=theme.dropdown_bg;v.label(3,3,"Rotate");v.cycler(Target::Rotate,14,3,9,ROTATIONS[settings.rotation]);v.toggle(Target::FlipH,3,4,"Flip horizontal",settings.flip_h);v.toggle(Target::FlipV,3,5,"Flip vertical",settings.flip_v);v.label(3,6,"Square");v.cycler(Target::Square,14,6,11,label(&SQUARES,&SQUARE_NAMES,settings.square));if settings.square==Square::Pad{v.pad(31,6);}v.help(3,9);v.button(Target::Done,37,9,6," Done ",theme.chip_go,true);},
        Popup::Source=>{let title=source_title(v.state);let alt=v.state.alternate.as_ref().map(|s|if s.len()==1{v.state.clicked.file_name().unwrap_or_default().to_string_lossy().into_owned()}else{format!("{} images",s.len())}).unwrap_or_default();v.radio(Target::SourceChoice,3,1,&dw::truncate_middle(&title,43),true);v.radio(Target::AlternateChoice,3,2,&dw::truncate_middle(&alt,43),false);v.button(Target::Done,43,4,6," Done ",theme.chip_go,true);},
        Popup::Color=>{v.label(3,1,"RGBA (hex)");let bg=ui::well(theme);v.state.hits.push(Hit::new(Target::ColorField,16,1,12,0));ui::fill(v.f,Rect::new(area.x+16,area.y+1,12,1),bg);let spans=super::inline_edit::render_text_input_value_with_style(&v.state.color_input,10,v.state.focus==Target::ColorField,ratatui::style::Style::default().fg(theme.value).bg(bg),theme);v.f.render_widget(ratatui::widgets::Paragraph::new(ratatui::text::Line::from(spans)),Rect::new(area.x+17,area.y+1,10,1));let rgba=v.state.color_value().unwrap_or_else(||v.state.effective_pad());let mut shown=rgba;if !v.state.format().alpha(){shown[3]=255;}v.swatch(32,1,6,shown,v.bg);v.soft(3,2,"Choose a channel or enter hexadecimal");for(c,name)in ["R","G","B","A"].into_iter().enumerate(){let y=4+c as u16;let line=super::theme_builder::slider_line(name,rgba[c],c,v.state.focus==Target::ColorChannel(c),true,theme);v.f.render_widget(ratatui::widgets::Paragraph::new(line),Rect::new(area.x+3,area.y+y,32,1));v.state.hits.push(Hit::new(Target::ColorChannel(c),8,y,18,0));v.button(Target::ColorStep(c,false),35,y,3," - ",theme.pill_neutral_bg,false);v.button(Target::ColorStep(c,true),40,y,3," + ",theme.pill_neutral_bg,false);}v.button(Target::ColorDefault,3,10,11," Automatic ",theme.pill_neutral_bg,false);v.button(Target::ColorDone,39,10,6," Done ",theme.chip_go,true);},
        Popup::Template=>{v.label(3,1,"Template");let bg=ui::well(theme);ui::fill(v.f,Rect::new(area.x+3,area.y+2,58,1),bg);v.state.hits.push(Hit::new(Target::TemplateField,3,2,58,0));let spans=super::inline_edit::render_text_input_value_with_style(&v.state.template_input,54,v.state.focus==Target::TemplateField,ratatui::style::Style::default().fg(theme.value).bg(bg),theme);v.f.render_widget(ratatui::widgets::Paragraph::new(ratatui::text::Line::from(spans)),Rect::new(area.x+4,area.y+2,54,1));v.label(3,4,"Tokens");let mut x=3;let mut y=5;for(i,token)in image_tokens(v.state.embedded).iter().enumerate(){let text=format!(" %{token}% ");let w=text.len() as u16;if x+w>61{x=3;y+=1;}v.button(Target::Token(i),x,y,w,&text,theme.pill_neutral_bg,false);x+=w+1;}v.label(3,7,"Separators");for(i,sep)in IMAGE_SEPARATORS.iter().enumerate(){let text=if *sep==" "{" sp ".into()}else{format!(" {sep} ")};v.button(Target::Separator(i),16+i as u16*5,7,4,&text,theme.pill_neutral_bg,false);}for(t,x,w,label)in [(Target::TemplateLeft,3,4," ← "),(Target::TemplateRight,8,4," → "),(Target::TemplateBackspace,13,12," Backspace "),(Target::TemplateClear,27,8," Clear ")]{v.button(t,x,8,w,label,theme.pill_neutral_bg,false);}v.button(Target::TemplateApply,35,10,9," Apply ",theme.chip_go,true);v.button(Target::TemplateCancel,46,10,11," Cancel ",theme.chip_dismiss,true);},
        Popup::Help=>{for(i,line)in ["↑ ↓   Move between rows","← →   Move between controls","Tab / Shift+Tab   Move between bands","[ ]   Change value or step image","Space   Select or toggle","Enter   Edit or press","Digits   Edit numeric value","Alt+O   OK (commit edits first)","p   Preview","Esc   Cancel edit, popup or dialog","Preview: Space   Before / After","Mouse: click any visible control"].iter().enumerate(){v.text(3,i as u16+1,line,theme.value);}v.button(Target::Done,51,15,6," Done ",theme.chip_go,true);},
    }
}
pub(super) fn draw(f:&mut Frame,s:&mut EditImageState,theme:Theme){
    if let Some(p)=s.preview.as_mut(){s.popup_area=f.size();p.draw(f,theme);return;}
    let screen=f.size();dim(f,screen,theme);draw_form(f,s,theme);
    if let Some(job)=s.job.as_mut(){job.progress.render(f,screen);return;}
    if let Some(folder)=s.folder.as_mut(){folder.render(f,screen);return;}
    draw_popup(f,s,theme);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend,Terminal};
    fn test_state()->EditImageState {let mut s=EditImageState::files("/tmp/cover.png".into(),vec![]);s.load=None;s.sources=vec![Source::File(s.clicked.clone())];s}
    #[test] fn fixed_cells_and_hits_across_modes_and_themes(){
        for theme in [crate::tui::theme::theme_by_slug_or_default("tokyo-night"),crate::tui::theme::theme_by_slug_or_default("tokyo-night-day")] {
            let mut s=test_state();let mut terminal=Terminal::new(TestBackend::new(80,24)).unwrap();
            for mode in MODES {s.settings.mode=mode;s.settings.format=Format::Jpeg;terminal.draw(|f|draw(f,&mut s,theme)).unwrap();let area=s.area;assert_eq!((area.width,area.height),(76,21));let buf=terminal.backend().buffer();assert_eq!(buf.get(area.x,area.y).symbol(),"┌");assert_eq!(buf.get(area.x,area.y+20).symbol(),"└");for y in [4,11,16,18]{assert_eq!(buf.get(area.x,area.y+y).symbol(),"├");}assert_eq!(buf.get(area.x+34,area.y+13).symbol(),"▸");assert_eq!(s.hits.iter().find(|h|h.target==Target::Browse).map(|h|(h.x,h.y,h.w)),Some((64,15,9)));assert_eq!(s.hits.iter().find(|h|h.target==Target::Ok).map(|h|(h.x,h.y,h.w)),Some((22,19,4)));}
            s.settings.format=Format::Png;terminal.draw(|f|draw(f,&mut s,theme)).unwrap();assert!(!s.hits.iter().any(|h|h.target==Target::Quality));
        }
    }
    #[test] fn custom_height_unlocks_ratio_and_quality_clamps(){let mut s=test_state();s.settings.mode=Mode::Size;s.settings.ratio=1;s.start_field(Target::H,None);s.edit.as_mut().unwrap().input.text="1350".into();s.commit_field();assert_eq!((s.settings.ratio,s.settings.height),(8,1352));s.settings.quality=98;s.cycle(Target::Quality,1);assert_eq!(s.settings.quality,100);s.cycle(Target::Quality,-1);assert_eq!(s.settings.quality,95);}
    #[test] fn embedded_write_visibility_is_fixed(){let mut s=test_state();s.embedded=true;s.settings=Settings::new(true);s.settings.write=WriteMode::Embed;let mut terminal=Terminal::new(TestBackend::new(80,24)).unwrap();terminal.draw(|f|draw(f,&mut s,crate::tui::theme::theme_by_slug_or_default("tokyo-night"))).unwrap();assert_eq!(s.area.height,22);assert!(!s.hits.iter().any(|h|matches!(h.target,Target::Browse|Target::Template|Target::Conflict)));assert_eq!(s.formats(),vec![Format::Jpeg,Format::Png]);}
    #[test] fn discover_ignores_corrupt_members_without_reordering(){let d=tempfile::tempdir().unwrap();let png=d.path().join("a.png");let jpg=d.path().join("b.jpg");let bad_jpg=d.path().join("bad.jpg");let bad_png=d.path().join("bad.png");let text=d.path().join("not-image.txt");image::DynamicImage::new_rgb8(8,8).save(&png).unwrap();image::DynamicImage::new_rgb8(8,8).save(&jpg).unwrap();std::fs::write(&bad_jpg,[0xff,0xd8,0xff,0xe0,0,16]).unwrap();std::fs::write(&bad_png,b"\x89PNG\r\n\x1a\n").unwrap();std::fs::write(&text,b"text").unwrap();assert!(pipe::supported_file(&bad_jpg));assert!(pipe::supported_file(&bad_png));let selection=vec![jpg.clone(),bad_jpg.clone(),text,bad_png,png.clone()];let loaded=discover_files(png.clone(),selection).unwrap();assert_eq!(loaded.primary,vec![Source::File(jpg),Source::File(png)]);assert_eq!(loaded.index,1);assert!(discover_files(bad_jpg,vec![]).is_err());}
    #[test] fn skip_gate_cannot_leak_across_items_or_publication(){let gate=SkipGate::default();gate.begin(1);assert!(gate.request(1));assert_eq!(gate.check(1),Err(SKIP_CURRENT.into()));assert_eq!(gate.seal(1),Err(SKIP_CURRENT.into()));gate.end(1);gate.begin(2);assert!(!gate.request(1));assert!(gate.check(2).is_ok());assert!(gate.seal(2).is_ok());assert!(!gate.request(2));gate.begin(3);assert!(gate.check(3).is_ok());}
    #[test] fn skip_during_conflict_does_not_skip_following_image(){let d=tempfile::tempdir().unwrap();let a=d.path().join("a.png");let b=d.path().join("b.png");for path in [&a,&b]{image::DynamicImage::new_rgb8(8,8).save(path).unwrap();}let conflict=d.path().join("a-out.png");std::fs::write(&conflict,b"existing").unwrap();let sources=vec![Source::File(a),Source::File(b)];let decoded=pipe::decode(&sources[0]).unwrap();let mut settings=Settings::new(false);settings.mode=Mode::Off;settings.template="%DIR%/%NAME%-out.%EXT%".into();let encoded=pipe::encode(&decoded,&settings).unwrap();let (tx,events)=mpsc::channel();let (_answer,answers)=mpsc::channel();let gate=SkipGate::default();let flags=(Arc::new(CancelFlag::new()),Arc::new(AtomicBool::new(false)),gate.clone());let worker=thread::spawn(move||{let mut totals=ProgressTotals::default();let result=run_job(&sources,0,&decoded,&encoded,&settings,&tx,&answers,&flags,&mut totals,&mut JobEffects::default());(result,totals)});let deadline=Instant::now()+Duration::from_secs(10);loop{match events.recv_timeout(deadline.saturating_duration_since(Instant::now())).unwrap(){JobEvent::Update(FileTaskProgressUpdate::ShowConflict{..})=>{assert!(gate.request(0));break;},_=>{}}}let(result,totals)=worker.join().unwrap();assert!(result.is_ok());assert_eq!((totals.completed,totals.skipped),(1,1));assert_eq!(std::fs::read(conflict).unwrap(),b"existing");assert!(d.path().join("b-out.png").is_file());assert_eq!(std::fs::read_dir(d.path()).unwrap().count(),4);}
    #[test] fn controls_keep_custom_values_and_complete_digits(){let app=AppState::new_for_test(crate::config::TonepoetConfig::default());let mut s=test_state();let theme=app.theme;let mut term=Terminal::new(TestBackend::new(80,24)).unwrap();let click=|s:&mut EditImageState,t:Target|{let h=s.hits.iter().find(|h|h.target==t).unwrap();let area=if s.popup.is_some(){s.popup_area}else{s.area};let m=MouseEvent{kind:MouseEventKind::Down(MouseButton::Left),column:area.x+h.x,row:area.y+h.y,modifiers:KeyModifiers::NONE};s.mouse(&app,m)};s.settings.mode=Mode::Long;term.draw(|f|draw(f,&mut s,theme)).unwrap();click(&mut s,Target::Custom);assert_eq!(s.presets[2],4);for c in "16384".chars(){s.key(&app,KeyEvent::new(KeyCode::Char(c),KeyModifiers::NONE));}s.key(&app,KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE));assert_eq!(s.settings.long,16384);term.draw(|f|draw(f,&mut s,theme)).unwrap();click(&mut s,Target::Preset(0));term.draw(|f|draw(f,&mut s,theme)).unwrap();click(&mut s,Target::Preset(4));assert!(s.edit.is_none());assert_eq!(s.settings.long,16384);s.settings.mode=Mode::Size;s.settings.ratio=7;s.settings.width=16384;s.settings.custom_ratio=(1920,1080);s.settings.portrait=true;term.draw(|f|draw(f,&mut s,theme)).unwrap();let buffer=term.backend().buffer();let cells=|x:u16,y:u16,w:u16|(x..x+w).map(|x|buffer.get(s.area.x+x,s.area.y+y).symbol().to_owned()).collect::<Vec<_>>().join("");assert!(cells(18,7,6).contains("16384"));assert!(cells(18,8,5).contains("1080"));assert!(cells(26,8,5).contains("1920"));}
    #[test] fn pad_and_template_popup_mouse_paths(){let app=AppState::new_for_test(crate::config::TonepoetConfig::default());let mut s=test_state();let theme=app.theme;let mut term=Terminal::new(TestBackend::new(80,24)).unwrap();let click=|s:&mut EditImageState,t:Target,end:bool|{let h=*s.hits.iter().find(|h|h.target==t).unwrap();let area=if s.popup.is_some(){s.popup_area}else{s.area};s.mouse(&app,MouseEvent{kind:MouseEventKind::Down(MouseButton::Left),column:area.x+h.x+if end{h.w-1}else{0},row:area.y+h.y,modifiers:KeyModifiers::NONE})};s.action(Target::Transform,&app);s.settings.square=Square::Pad;term.draw(|f|draw(f,&mut s,theme)).unwrap();click(&mut s,Target::Pad,false);term.draw(|f|draw(f,&mut s,theme)).unwrap();click(&mut s,Target::ColorChannel(0),true);click(&mut s,Target::ColorDone,false);assert_eq!(s.settings.pad,Some([255,0,0,0]));s.close_popup();term.draw(|f|draw(f,&mut s,theme)).unwrap();click(&mut s,Target::Template,false);term.draw(|f|draw(f,&mut s,theme)).unwrap();click(&mut s,Target::TemplateClear,false);click(&mut s,Target::Token(1),false);click(&mut s,Target::Separator(3),false);click(&mut s,Target::Token(2),false);click(&mut s,Target::TemplateApply,false);assert_eq!(s.settings.template,"%NAME%.%EXT%");}

    #[test]
    fn destination_fallback_manual_abort_preserves_completed_batch() {
        use std::fs;
        use super::super::file_task_runtime::{recovery_surface_records, RecoverySurfaceAvailability};
        const FIXTURE: &str = "TONEPOET_EDIT_IMAGE_FALLBACK_FIXTURE";
        const TEST: &str = "tui::edit_image::tests::destination_fallback_manual_abort_preserves_completed_batch";
        let Some(root) = std::env::var_os(FIXTURE).map(PathBuf::from) else {
            let root = tempfile::tempdir().unwrap();
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--exact").arg(TEST).arg("--nocapture")
                .env(FIXTURE, root.path())
                .env("TONEPOET_FILE_OPERATION_JOURNAL_DIR", root.path().join("journal"))
                .env("TONEPOET_CONCURRENCY_DIR", root.path().join("coordination"))
                .env("TONEPOET_TEST_CONCURRENCY_DIR_INHERIT", "1")
                .env("TONEPOET_EDIT_IMAGE_FALLBACK_PHASE", "writing")
                .env("TONEPOET_EDIT_IMAGE_FALLBACK_AFTER", root.path().join("album/a-edited.png"))
                .stdin(std::process::Stdio::null())
                .status().unwrap().success());
            return;
        };
        let album = root.join("album"); fs::create_dir(&album).unwrap();
        let names = || fs::read_dir(&album).unwrap().map(|e| e.unwrap().file_name()).collect::<std::collections::BTreeSet<_>>();
        fs::write(album.join(".keep"), b"unrelated").unwrap();
        fs::create_dir(album.join("unrelated-dir")).unwrap();
        let sources: Vec<_> = ["a.png", "b.png", "c.png"].into_iter().map(|name| {
            let path = album.join(name); image::DynamicImage::new_rgb8(32, 24).save(&path).unwrap(); Source::File(path)
        }).collect();
        let originals: Vec<_> = sources.iter().map(|source| fs::read(source.path()).unwrap()).collect();
        let before = names(); let input = pipe::decode(&sources[0]).unwrap();
        let mut settings = Settings::new(false); settings.mode = Mode::Off;
        settings.format = Format::Png; settings.template = "%DIR%/%NAME%-edited.%EXT%".into();
        let encoded = pipe::encode(&input, &settings).unwrap();
        let theme = crate::tui::theme::theme_by_slug_or_default("tokyo-night");
        let mut editor = loaded_state(sources[0].path());
        editor.job = Some(Job::start(sources.clone(), 0, input, encoded.clone(), settings.clone(), theme));
        let deadline = Instant::now() + Duration::from_secs(20);
        while !root.join("ready").is_file() {
            assert!(Instant::now() < deadline, "batch did not enter second staged write");
            thread::sleep(Duration::from_millis(5));
        }
        let stage = PathBuf::from(fs::read_to_string(root.join("ready")).unwrap());
        assert!(stage.is_dir()); assert!(!fs::read_dir(&stage).unwrap().collect::<Vec<_>>().is_empty());
        let first = album.join("a-edited.png"); assert_eq!(fs::read(&first).unwrap(), *encoded.bytes);
        let mut expected = before.clone(); expected.insert(first.file_name().unwrap().to_owned());
        let mut during = expected.clone(); during.insert(stage.file_name().unwrap().to_owned()); assert_eq!(names(), during);
        let records = recovery_surface_records(); assert_eq!(records.len(), 1);
        let entry = &records[0]; assert_eq!(entry.availability, RecoverySurfaceAvailability::Live);
        assert!(entry.record.as_ref().unwrap().is_edit_image_cleanup());
        // Use the editor's normal operation-control path, not a pre-cancelled flag.
        // Its worker is still parked with an open staged file at this point.
        assert!(editor.cancel_running());
        assert_eq!(editor.job.as_ref().unwrap().progress.phase, FileTaskPhase::Cancelling);
        assert!(stage.is_dir()); assert_eq!(names(), during);
        fs::write(root.join("release"), b"continue").unwrap();
        let wait_done = |job: &Job| {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                match job.events.poll() {
                    Ok(JobEvent::Done(result, totals, effects)) => break (result, totals, effects),
                    Ok(_) | Err(mpsc::TryRecvError::Empty) => {},
                    Err(error) => panic!("worker disconnected: {error}"),
                }
                assert!(Instant::now() < deadline, "Edit Image worker did not stop");
                thread::sleep(Duration::from_millis(5));
            }
        };
        let (result, totals, effects) = wait_done(editor.job.as_ref().unwrap());
        assert!(result.unwrap_err().aborted);
        assert_eq!((totals.completed, totals.errors, totals.not_attempted), (1, 1, 1));
        assert_eq!((effects.files_written, effects.tracks_updated, effects.cleanup_pending), (1, 0, 0));
        assert!(!stage.exists()); assert_eq!(names(), expected);
        assert_eq!(fs::read(&first).unwrap(), *encoded.bytes);
        for (source, old) in sources.iter().zip(&originals) { assert_eq!(&fs::read(source.path()).unwrap(), old); }
        assert!(recovery_surface_records().is_empty());
        // Start another real job in the same process after Abort.
        editor.job = None;
        let input = pipe::decode(&sources[1]).unwrap(); let encoded = pipe::encode(&input, &settings).unwrap();
        editor.job = Some(Job::start(vec![sources[1].clone()], 0, input, encoded.clone(), settings.clone(), theme));
        let (result, totals, effects) = wait_done(editor.job.as_ref().unwrap());
        assert!(result.is_ok()); assert_eq!(totals.completed, 1); assert_eq!(effects.files_written, 1);
        assert_eq!(fs::read(album.join("b-edited.png")).unwrap(), *encoded.bytes);
        expected.insert("b-edited.png".into()); assert_eq!(names(), expected);
        assert!(recovery_surface_records().is_empty());
        // A later source whose requested dimensions exceed the existing
        // encoder admission bound must fail without discarding earlier output.
        let too_large = album.join("oversized.png");
        image::DynamicImage::new_rgb8(17000, 1).save(&too_large).unwrap();
        expected.insert("oversized.png".into());
        let mut failure_settings = settings; failure_settings.mode = Mode::Scale;
        failure_settings.scale = 2.0; failure_settings.no_upscale = false;
        let input = pipe::decode(&sources[2]).unwrap();
        let encoded = pipe::encode(&input, &failure_settings).unwrap();
        let job = Job::start(vec![sources[2].clone(), Source::File(too_large)], 0, input, encoded.clone(), failure_settings, theme);
        let (result, totals, effects) = wait_done(&job);
        assert!(result.is_err()); assert_eq!((totals.completed, totals.errors), (1, 1));
        assert_eq!((effects.files_written, effects.cleanup_pending), (1, 0));
        assert_eq!(fs::read(album.join("c-edited.png")).unwrap(), *encoded.bytes);
        expected.insert("c-edited.png".into()); assert_eq!(names(), expected);
        assert!(recovery_surface_records().is_empty());
    }

    fn click_target(app:&AppState,s:&mut EditImageState,target:Target){let hit=*s.hits.iter().find(|h|h.target==target).expect("visible target");let area=if s.popup.is_some(){s.popup_area}else{s.area};assert!(s.mouse(app,MouseEvent{kind:MouseEventKind::Down(MouseButton::Left),column:area.x+hit.x,row:area.y+hit.y,modifiers:KeyModifiers::NONE}));}
    fn press(app:&AppState,s:&mut EditImageState,code:KeyCode){assert!(s.key(app,KeyEvent::new(code,KeyModifiers::NONE)));}
    fn loaded_state(path:&Path)->EditImageState{let mut s=test_state();s.clicked=path.to_path_buf();let src=Source::File(path.to_path_buf());s.sources=vec![src.clone()];s.decoded=Some((src.clone(),pipe::decode(&src).unwrap()));s}
    #[test]
    fn round3_title_picker_is_drawn_only_for_two_source_choices() {
        let app = AppState::new_for_test(crate::config::TonepoetConfig::default());
        let mut term = Terminal::new(TestBackend::new(80,24)).unwrap();
        for slug in ["tokyo-night", "tokyo-night-day"] {
            let theme = crate::tui::theme::theme_by_slug_or_default(slug);
            let mut s = test_state();
            let title = |term: &Terminal<TestBackend>, area: Rect| (0..area.width)
                .map(|x| term.backend().buffer().get(area.x+x,area.y).symbol()).collect::<String>();
            let click = |s: &mut EditImageState, x| {
                assert!(s.mouse(&app, MouseEvent { kind: MouseEventKind::Down(MouseButton::Left),
                    column: s.area.x+x, row: s.area.y, modifiers: KeyModifiers::NONE }));
            };
            term.draw(|f| draw(f,&mut s,theme)).unwrap();
            let row = title(&term,s.area);
            assert!(row.starts_with("┌  edit image"));assert!(row.contains("cover.png"));
            for glyph in ['[','▾','◆',']','▸'] { assert!(!row.contains(glyph), "{slug}: {row}"); }
            assert!(!s.hits.iter().any(|h| h.y == 0));
            click(&mut s, 1);click(&mut s,15);assert!(s.popup.is_none());
            s.alternate=Some(vec![Source::File(s.clicked.clone()),Source::File("/tmp/other.png".into())]);
            term.draw(|f| draw(f,&mut s,theme)).unwrap();
            assert!(title(&term,s.area).contains("[▾ cover.png ◆]"));
            assert!(!title(&term,s.area).contains('▸'));
            let hit = *s.hits.iter().find(|h| h.target==Target::Picker).unwrap();
            assert_eq!((hit.x,hit.y,hit.w),(15,0,15));
            for x in [hit.x,hit.x+1,hit.x+hit.w-2,hit.x+hit.w-1] {
                click(&mut s,x);assert_eq!(s.popup,Some(Popup::Source));
                s.close_popup();term.draw(|f| draw(f,&mut s,theme)).unwrap();
            }
            click(&mut s,1);assert!(s.popup.is_none());
            click_target(&app,&mut s,Target::Picker);
            term.draw(|f| draw(f,&mut s,theme)).unwrap();
            click_target(&app,&mut s,Target::AlternateChoice);
            term.draw(|f| draw(f,&mut s,theme)).unwrap();
            assert!(title(&term,s.area).contains("[▾ 2 images ◆]"));
            s.alternate=None; // Also cover a multi-image selection with no alternate choice.
            term.draw(|f| draw(f,&mut s,theme)).unwrap();
            assert!(!title(&term,s.area).contains('▾'));
            assert!(!s.hits.iter().any(|h| h.target==Target::Picker));
            assert_ne!(s.focus,Target::Picker);
        }
    }

    #[test]
    fn ordinary_format_cycle_and_independent_webp_quality() {
        let app=AppState::new_for_test(crate::config::TonepoetConfig::default());
        let mut s=test_state();
        let formats=[Format::Same,Format::Jpeg,Format::Png,Format::WebpLossy,Format::Webp,Format::Gif,Format::Bmp];
        assert_eq!(s.formats(),formats);
        let mut term=Terminal::new(TestBackend::new(80,24)).unwrap();
        for format in formats {
            assert_eq!(s.settings.format,format);
            term.draw(|f| draw(f,&mut s,app.theme)).unwrap();
            assert_eq!(s.hits.iter().any(|h|h.target==Target::Quality),matches!(format,Format::Jpeg|Format::WebpLossy));
            s.cycle(Target::Format,1);
        }
        assert_eq!(s.settings.format,Format::Same);
        s.settings.format=Format::WebpLossy;
        assert_eq!(s.quality(),85);
        s.focus=Target::Quality;term.draw(|f| draw(f,&mut s,app.theme)).unwrap();
        press(&app,&mut s,KeyCode::Char(']'));assert_eq!(s.quality(),90);
        press(&app,&mut s,KeyCode::Char('7'));press(&app,&mut s,KeyCode::Char('3'));press(&app,&mut s,KeyCode::Enter);assert_eq!(s.quality(),73);
        s.settings.format=Format::Jpeg;assert_eq!(s.quality(),90);s.cycle(Target::Quality,-1);assert_eq!(s.quality(),85);
        s.settings.format=Format::WebpLossy;assert_eq!(s.quality(),73);
        term.draw(|f|draw(f,&mut s,app.theme)).unwrap();
        assert!(s.mouse(&app,MouseEvent{kind:MouseEventKind::Down(MouseButton::Left),column:s.area.x+66,row:s.area.y+13,modifiers:KeyModifiers::NONE}));
        assert_eq!(s.settings.webp_quality,60);assert_eq!(s.settings.quality,85);
        s.embedded=true;s.settings.write=WriteMode::Both;assert_eq!(s.formats(),vec![Format::Jpeg,Format::Png]);
    }

    #[test]
    fn edit_preview_native_protocols_toggle_and_kitty_retries_withheld_upload() {
        use ratatui_image::picker::{Picker,ProtocolType};
        let d=tempfile::tempdir().unwrap();let path=d.path().join("cover.png");
        image::DynamicImage::new_rgb8(192,128).save(&path).unwrap();
        let app=AppState::new_for_test(crate::config::TonepoetConfig::default());
        for protocol in [ProtocolType::Kitty,ProtocolType::Sixel] {
            let mut s=loaded_state(&path);s.settings.mode=Mode::Long;s.settings.long=96;
            s.settings.format=Format::WebpLossy;
            s.encoded=Some(pipe::encode(s.current().unwrap(),&s.settings).unwrap());
            let mut picker=Picker::new((8,12));picker.protocol_type=protocol;
            let mut term=Terminal::new(TestBackend::new(80,24)).unwrap();
            press(&app,&mut s,KeyCode::Char('p'));s.prepare(&mut picker,0,0,app.theme);
            assert!(s.preview.is_some());
            for after in [true,false,true] {
                if s.preview.as_ref().unwrap().show_after!=after {press(&app,&mut s,KeyCode::Char(' '));}
                let deadline=Instant::now()+Duration::from_secs(15);
                // Draw sets the real preview viewport; do not consume the
                // first ready native frame before inspecting its upload.
                loop {
                    s.prepare(&mut picker,0,0,app.theme);
                    if s.preview.as_ref().unwrap().viewer.lock().unwrap().native_frame_ready() {break;}
                    assert!(Instant::now()<deadline,"{protocol:?} preview worker did not complete");
                    term.draw(|f|draw(f,&mut s,app.theme)).unwrap();
                    thread::sleep(Duration::from_millis(2));
                }
                let p=s.preview.as_ref().unwrap();assert!(p.error.is_none());assert_eq!(p.show_after,after);
                assert_eq!((p.before.width(),p.before.height()),(192,128));
                let pixels=p.after.as_ref().unwrap();assert_eq!((pixels.width(),pixels.height()),(96,64));
                if protocol==ProtocolType::Kitty {
                    term.draw(|f|{
                        draw(f,&mut s,app.theme);
                        assert!(crate::tui::kitty_transport::buffer_has_kitty_upload(f.buffer_mut()));
                        crate::tui::kitty_transport::suppress_unverified_tmux_graphics(f.buffer_mut());
                        s.kitty_upload_withheld();
                    }).unwrap();
                    term.draw(|f|{draw(f,&mut s,app.theme);assert!(crate::tui::kitty_transport::buffer_has_kitty_upload(f.buffer_mut()));}).unwrap();
                    term.draw(|f|{draw(f,&mut s,app.theme);assert!(!crate::tui::kitty_transport::buffer_has_kitty_upload(f.buffer_mut()));}).unwrap();
                } else {term.draw(|f|draw(f,&mut s,app.theme)).unwrap();}
            }
            press(&app,&mut s,KeyCode::Esc);assert!(s.preview.is_none());assert!(!s.wrote);
            assert_eq!(std::fs::read_dir(d.path()).unwrap().count(),1);
        }
    }

    #[test]
    fn quality_stepper_is_one_logical_keyboard_and_mouse_control() {
        let d=tempfile::tempdir().unwrap();
        let first=d.path().join("first.jpg");let second=d.path().join("second.jpg");
        for path in [&first,&second]{image::DynamicImage::new_rgb8(24,16).save(path).unwrap();}
        let app=AppState::new_for_test(crate::config::TonepoetConfig::default());
        for slug in ["tokyo-night","tokyo-night-day"] {
            let theme=crate::tui::theme::theme_by_slug_or_default(slug);
            for batch in [false,true] {
                let mut s=loaded_state(&first);
                if batch{s.sources.push(Source::File(second.clone()));}
                s.settings.format=Format::Jpeg;s.settings.quality=90;s.focus=Target::Format;
                let mut term=Terminal::new(TestBackend::new(80,24)).unwrap();
                term.draw(|f|draw(f,&mut s,theme)).unwrap();
                let hits:Vec<_>=s.hits.iter().filter(|h|h.target==Target::Quality).map(|h|(h.x,h.y,h.w)).collect();
                assert_eq!(hits,vec![(51,13,9)]);
                press(&app,&mut s,KeyCode::Right);assert_eq!(s.focus,Target::Quality);
                press(&app,&mut s,KeyCode::Char(']'));assert_eq!((s.settings.quality,s.index),(95,0));
                press(&app,&mut s,KeyCode::Char('['));assert_eq!((s.settings.quality,s.index),(90,0));
                // The bar must not become a second keyboard stop.
                press(&app,&mut s,KeyCode::Right);assert_eq!(s.focus,Target::Quality);
                term.draw(|f|draw(f,&mut s,theme)).unwrap();
                let area=s.area;
                let buffer=term.backend().buffer();
                assert_eq!((51..60).map(|x|buffer.get(area.x+x,area.y+13).symbol()).collect::<String>(),"◂   90  ▸");
                for x in 51..60 {
                    let expected=if (53..=57).contains(&x){ui::well(theme)}else{theme.selection_bg};
                    assert_eq!(buffer.get(area.x+x,area.y+13).bg,expected,"focused stepper cell {x} in {slug}");
                }
                press(&app,&mut s,KeyCode::Left);assert_eq!(s.focus,Target::Format);
                press(&app,&mut s,KeyCode::Right);assert_eq!(s.focus,Target::Quality);
                press(&app,&mut s,KeyCode::Enter);assert!(s.edit.is_some());
                for c in "92".chars(){press(&app,&mut s,KeyCode::Char(c));}
                press(&app,&mut s,KeyCode::Enter);assert_eq!((s.settings.quality,s.index),(92,0));
                press(&app,&mut s,KeyCode::Char('['));assert_eq!(s.settings.quality,87);
                press(&app,&mut s,KeyCode::Char(']'));press(&app,&mut s,KeyCode::Char(']'));assert_eq!(s.settings.quality,97);
                press(&app,&mut s,KeyCode::Char(']'));press(&app,&mut s,KeyCode::Char(']'));assert_eq!(s.settings.quality,100);
                press(&app,&mut s,KeyCode::Char('1'));press(&app,&mut s,KeyCode::Enter);
                press(&app,&mut s,KeyCode::Char('['));assert_eq!((s.settings.quality,s.index),(1,0));
                press(&app,&mut s,KeyCode::Char('8'));press(&app,&mut s,KeyCode::Esc);assert_eq!(s.settings.quality,1);
                let mouse=|s:&mut EditImageState,x:u16,kind:MouseEventKind|{
                    assert!(s.mouse(&app,MouseEvent{kind,column:s.area.x+x,row:s.area.y+13,modifiers:KeyModifiers::NONE}));
                };
                let click=MouseEventKind::Down(MouseButton::Left);
                s.settings.quality=92;
                mouse(&mut s,51,click);assert_eq!((s.settings.quality,s.focus,s.index),(87,Target::Quality,0));
                s.settings.quality=92;
                mouse(&mut s,59,click);assert_eq!((s.settings.quality,s.focus,s.index),(97,Target::Quality,0));
                mouse(&mut s,59,click);mouse(&mut s,59,click);assert_eq!(s.settings.quality,100);
                for (x,value) in [(61,10),(65,50),(70,100)] {
                    mouse(&mut s,x,click);assert_eq!((s.settings.quality,s.focus,s.index),(value,Target::Quality,0));
                }
                mouse(&mut s,55,click);assert_eq!(s.edit.as_ref().map(|e|e.target),Some(Target::Quality));
                press(&app,&mut s,KeyCode::Char('2'));press(&app,&mut s,KeyCode::Esc);assert_eq!(s.settings.quality,100);
                mouse(&mut s,55,click);press(&app,&mut s,KeyCode::Char('2'));
                mouse(&mut s,51,click);assert!(s.edit.is_none());assert_eq!(s.settings.quality,1);
                s.focus=Target::Format;mouse(&mut s,70,MouseEventKind::Moved);
                term.draw(|f|draw(f,&mut s,theme)).unwrap();
                let buffer=term.backend().buffer();
                for x in 51..60 {
                    let expected=if (53..=57).contains(&x){ui::well(theme)}else{theme.hover_bg};
                    assert_eq!(buffer.get(area.x+x,area.y+13).bg,expected,"hovered stepper cell {x} in {slug}");
                }
                s.focus=Target::Transform;
                press(&app,&mut s,KeyCode::Char(']'));assert_eq!(s.index,usize::from(batch));
                press(&app,&mut s,KeyCode::Char('['));assert_eq!(s.index,0);
                assert_eq!(s.settings.quality,1);
                s.settings.format=Format::Png;
                term.draw(|f|draw(f,&mut s,theme)).unwrap();
                assert!(!s.hits.iter().any(|h|matches!(h.target,Target::Quality|Target::QualityBar)));
            }
        }
    }

    #[test]
    fn edit_image_mockup_cell_colours_in_both_themes() {
        use crate::tui::theme::{contrast_ratio,rgb_tuple,theme_by_slug_or_default};
        fn mix_reference(a:Color,b:Color,n:u32,d:u32)->Color {
            let (ar,ag,ab)=rgb_tuple(a);let (br,bg,bb)=rgb_tuple(b);
            let ch=|x:u8,y:u8|((u32::from(x)*(d-n)+u32::from(y)*n)/d) as u8;
            Color::Rgb(ch(ar,br),ch(ag,bg),ch(ab,bb))
        }
        fn readable_reference(fg:Color,bg:Color)->Color {
            // The guide requires the closest legible interpolation toward
            // white or black. Exhaustive search is independent of UI's binary search.
            if contrast_ratio(fg,bg)>=4.5{return fg;}
            for amount in 1..=256 {
                for pole in [Color::Rgb(255,255,255),Color::Rgb(0,0,0)] {
                    let c=mix_reference(fg,pole,amount,256);
                    if contrast_ratio(c,bg)>=4.5{return c;}
                }
            }
            panic!("no legible colour for {fg:?} on {bg:?}");
        }
        let d=tempfile::tempdir().unwrap();let path=d.path().join("sleeve-scan.png");
        image::DynamicImage::new_rgb8(2400,1800).save(&path).unwrap();
        for slug in ["tokyo-night","tokyo-night-day"] {
            let t=theme_by_slug_or_default(slug);
            let (panel,rest,active,well,popup,neutral)=if t.dark {
                (Color::Rgb(0x1a,0x1b,0x26),Color::Rgb(0x25,0x28,0x39),Color::Rgb(0x2a,0x2e,0x43),Color::Rgb(0x11,0x12,0x19),Color::Rgb(0x22,0x24,0x34),Color::Rgb(0x43,0x46,0x59))
            }else{
                (Color::Rgb(0xe1,0xe2,0xe7),Color::Rgb(0xd7,0xd9,0xe2),Color::Rgb(0xd2,0xd5,0xe0),Color::Rgb(0xf5,0xf5,0xf7),Color::Rgb(0xd9,0xdb,0xe3),Color::Rgb(0xa8,0xb6,0xd9))
            };
            // The Day mockup was authored with older illustrative accents.
            // Spec section 12 explicitly makes the current theme.rs authoritative
            // for Day roles. Do not introduce a second Day palette in the editor.
            let soft=mix_reference(t.label,t.value,1,2);
            if t.dark {assert_eq!(soft,Color::Rgb(0x9f,0xa9,0xd4));}
            let mut term=Terminal::new(TestBackend::new(80,24)).unwrap();
            let check=|term:&Terminal<TestBackend>,area:Rect,state:&str,x:u16,y:u16,symbol:&str,fg:Color,bg:Color,word:bool|{
                let c=term.backend().buffer().get(area.x+x,area.y+y);
                let expected=if word{readable_reference(fg,bg)}else{fg};
                assert_eq!(c.symbol(),symbol,"{slug} {state} ({x},{y}) symbol");
                assert_eq!((c.fg,c.bg),(expected,bg),"{slug} {state} ({x},{y}) colour");
                if word {assert!(contrast_ratio(c.fg,c.bg)>=4.5,"{slug} {state} ({x},{y}) contrast");}
            };
            // Expected runs come directly from mockups A/B, C and D, not
            // from the production token scanner. Check both delimiters and
            // every intervening character, as well as literal separators.
            let check_template=|term:&Terminal<TestBackend>,area:Rect,state:&str,y:u16,runs:&[(&str,Color)]|{
                let mut x=17;
                for &(run,fg) in runs {
                    for ch in run.chars() {
                        let glyph=ch.to_string();
                        check(term,area,state,x,y,&glyph,fg,well,true);
                        x+=dw::width(&glyph) as u16;
                    }
                }
            };
            let file_template=[("%DIR%",t.purple),("/",t.value),("%NAME%",t.purple),("_",t.value),("%W%",t.purple),("x",t.value),("%H%",t.purple),(".",t.value),("%EXT%",t.purple)];
            let embedded_template=[("%ALBUMDIR%",t.purple),("/cover.",t.value),("%EXT%",t.purple)];
            let selected_template=[("~/scans/upright/",t.value),("%NAME%",t.purple),(".",t.value),("%EXT%",t.purple)];
            let mut s=loaded_state(&path);s.settings.format=Format::Jpeg;s.focus=Target::Preset(2);
            // A: one image, Long edge, focused Resize region, JPEG controls.
            term.draw(|f|draw(f,&mut s,t)).unwrap();let a=s.area;
            check(&term,a,"A",0,0,"┌",t.title,panel,false);
            check(&term,a,"A",3,0,"e",panel,t.title,true);
            check(&term,a,"A",3,1,"S",t.label,rest,true);
            check(&term,a,"A",16,2,"s",t.value,rest,true);
            check(&term,a,"A",4,3,"T",t.value,neutral,true);
            check(&term,a,"A",16,3,"n",soft,rest,true);
            check(&term,a,"A",3,5,"R",t.header,active,true);
            check(&term,a,"A",16,6,"○",t.label,active,true);
            check(&term,a,"A",43,6,"●",t.info,active,true);
            let preset=*s.hits.iter().find(|h|h.target==Target::Preset(2)).unwrap();
            check(&term,a,"A",preset.x,preset.y,"●",t.info,t.selection_bg,true);
            check(&term,a,"A",16,8,"1",soft,active,true);
            check(&term,a,"A",41,10,"☑",t.info,active,true);
            check(&term,a,"A",16,13,"◂",t.info,rest,true);
            check(&term,a,"A",55,13,"9",t.value,well,true);
            check(&term,a,"A",61,13,"█",t.info,rest,false);
            check(&term,a,"A",70,13,"░",t.text_dim,rest,false);
            check(&term,a,"A",17,15,"%",t.purple,well,true);
            check_template(&term,a,"A template",15,&file_template);
            check(&term,a,"A",23,19,"O",panel,t.chip_go,true);
            check(&term,a,"A",29,19,"p",panel,t.tab_active,true);
            check(&term,a,"A",42,19,"E",panel,t.chip_dismiss,true);
            check(&term,a,"A",63,19,"?",t.info,panel,true);
            // B: a read-only H and focused Ratio stepper, computed fill note.
            s.settings.mode=Mode::Size;s.settings.ratio=1;s.settings.portrait=true;s.focus=Target::Ratio;
            term.draw(|f|draw(f,&mut s,t)).unwrap();
            check(&term,a,"B",30,7,"1",soft,well,true);
            check(&term,a,"B",16,8,"◂",t.info,t.selection_bg,true);
            check(&term,a,"B",18,8,"4",t.value,t.selection_bg,true);
            check(&term,a,"B",53,8,"●",t.info,active,true);
            check(&term,a,"B",40,9,"t",soft,active,true);
            check_template(&term,a,"B template",15,&file_template);
            // C: render a two-picture model without mutating a real audio tag.
            let mut c=loaded_state(&path);c.embedded=true;c.settings=Settings::new(true);
            c.clicked=d.path().join("album.flac");c.settings.write=WriteMode::Both;c.settings.square=Square::Crop;c.focus=Target::Write;
            let picture=|index,kind|Source::Picture{path:c.clicked.clone(),tag:Some(lofty::tag::TagType::VorbisComments),index,kind,description:None,digest:[0;32],album:"Album".into(),artist:"Artist".into()};
            c.sources=vec![picture(0,lofty::picture::PictureType::CoverFront),picture(1,lofty::picture::PictureType::CoverBack)];
            let decoded=c.decoded.take().unwrap().1;c.decoded=Some((c.sources[0].clone(),decoded));
            term.draw(|f|draw(f,&mut c,t)).unwrap();let ca=c.area;
            check(&term,ca,"C",16,2,"◂",t.info,rest,true);
            check(&term,ca,"C",44,2,"1",soft,rest,true);
            check(&term,ca,"C",16,3,"S",t.value,rest,true);
            check(&term,ca,"C",3,12,"O",t.header,active,true);
            check(&term,ca,"C",16,15,"◂",t.info,t.selection_bg,true);
            check(&term,ca,"C",18,15,"F",t.value,t.selection_bg,true);
            check(&term,ca,"C",17,16,"%",t.purple,well,true);
            check_template(&term,ca,"C template",16,&embedded_template);
            check(&term,ca,"C",23,20,"O",panel,t.chip_go,true);
            // D: source selection, resize off, PNG: absent quality/filter cells.
            let mut s=loaded_state(&path);s.sources=(0..12).map(|i|Source::File(d.path().join(format!("scan-{i:02}.png")))).collect();
            s.index=2;s.alternate=Some(vec![Source::File(path.clone())]);let decoded=s.decoded.take().unwrap().1;s.decoded=Some((s.sources[2].clone(),decoded));
            s.settings.mode=Mode::Off;s.settings.rotation=1;s.settings.format=Format::Png;s.settings.conflict=Conflict::Skip;s.settings.template="~/scans/upright/%NAME%.%EXT%".into();s.focus=Target::Image;
            term.draw(|f|draw(f,&mut s,t)).unwrap();let a=s.area;
            check(&term,a,"D",3,1,"S",t.header,active,true);
            check(&term,a,"D",16,2,"◂",t.info,t.selection_bg,true);
            check(&term,a,"D",16,3,"R",t.value,active,true);
            check(&term,a,"D",16,6,"●",t.info,rest,true);
            check(&term,a,"D",17,15,"~",t.value,well,true);
            check_template(&term,a,"D template",15,&selected_template);
            for x in 51..=70 {let c=term.backend().buffer().get(a.x+x,a.y+13);assert_eq!((c.symbol(),c.bg),(" ",rest));}
            assert!(!s.hits.iter().any(|h|matches!(h.target,Target::Quality|Target::QualityBar|Target::Filter)));
            // Additional boundaries: adjacent tokens, a trailing literal,
            // an unfinished token and a filename without tokens.
            for (template,runs) in [
                ("%NAME%%EXT%-copy", &[("%NAME%",t.purple),("%EXT%",t.purple),("-copy",t.value)][..]),
                ("cover.%EX", &[("cover.",t.value),("%EX",t.purple)][..]),
                ("cover.png", &[("cover.png",t.value)][..]),
            ] {
                s.settings.template=template.into();
                term.draw(|f|draw(f,&mut s,t)).unwrap();
                check_template(&term,s.area,template,15,runs);
            }
            // E: fixed popup with focused Square and attenuated parent bands.
            let mut s=loaded_state(&path);s.settings.format=Format::Jpeg;s.settings.rotation=1;s.settings.square=Square::Crop;s.focus=Target::Transform;s.popup_open(Popup::Transform,Target::Square);
            term.draw(|f|draw(f,&mut s,t)).unwrap();let a=s.area;let pa=s.popup_area;
            check(&term,a,"E parent",0,0,"┌",t.text_dim,panel,false);
            check(&term,a,"E parent",3,0,"e",t.text_dim,mix_reference(panel,t.title,1,3),false);
            check(&term,a,"E parent",3,1,"S",t.text_dim,mix_reference(panel,active,1,3),false);
            check(&term,pa,"E",0,0,"┌",t.title,panel,false);
            check(&term,pa,"E",3,0,"T",panel,t.title,true);
            check(&term,pa,"E",3,1,"A",soft,rest,true);
            check(&term,pa,"E",3,3,"R",t.label,popup,true);
            check(&term,pa,"E",14,3,"◂",t.info,popup,true);
            check(&term,pa,"E",16,3,"9",t.value,popup,true);
            check(&term,pa,"E",4,4,"☐",t.label,popup,true);
            check(&term,pa,"E",7,4,"F",t.value,popup,true);
            check(&term,pa,"E",14,6,"◂",t.info,t.selection_bg,true);
            check(&term,pa,"E",16,6,"C",t.value,t.selection_bg,true);
            check(&term,pa,"E",3,9,"?",t.info,popup,true);
            check(&term,pa,"E",38,9,"D",panel,t.chip_go,true);
            // G: format/ratio geometry, custom wells, opaque pad swatch.
            s.close_popup();s.settings.rotation=0;s.settings.square=Square::Off;s.settings.mode=Mode::Size;s.focus=Target::Transform;
            for format in [Format::Same,Format::Jpeg,Format::Webp,Format::WebpLossy] {
                s.settings.format=format;term.draw(|f|draw(f,&mut s,t)).unwrap();let a=s.area;
                check(&term,a,"G format",16,13,"◂",t.info,rest,true);
                check(&term,a,"G format",34,13,"▸",t.info,rest,true);
                let first=format.label().chars().next().unwrap().to_string();
                check(&term,a,"G format",18,13,&first,t.value,rest,true);
            }
            s.settings.format=Format::Jpeg;
            for ratio in [0,5,8,7] {
                s.settings.ratio=ratio;s.settings.custom_ratio=(7,5);s.settings.width=1080;s.settings.height=592;s.settings.fill=Fill::Pad;
                term.draw(|f|draw(f,&mut s,t)).unwrap();let a=s.area;
                check(&term,a,"G ratio",16,8,"◂",t.info,rest,true);
                check(&term,a,"G ratio",32,8,"▸",t.info,rest,true);
                if ratio==7 {
                    check(&term,a,"G custom ratio",21,8,"7",t.value,well,true);
                    check(&term,a,"G custom ratio",24,8,":",soft,rest,true);
                    check(&term,a,"G custom ratio",29,8,"5",t.value,well,true);
                    check(&term,a,"G pad",34,9,"▕",t.label,rest,true);
                    check(&term,a,"G pad",35,9,"█",Color::Rgb(0,0,0),rest,false);
                    check(&term,a,"G pad",36,9,"█",Color::Rgb(0,0,0),rest,false);
                    check(&term,a,"G pad",37,9,"▏",t.label,rest,true);
                }else{
                    let first=match ratio{0=>"S",5=>"1",8=>"F",_=>unreachable!()};
                    check(&term,a,"G ratio",18,8,first,t.value,rest,true);
                }
            }
        }
    }

    #[test]
    fn size_quality_and_transform_overlap_match_mockup_cells() {
        let d=tempfile::tempdir().unwrap();let path=d.path().join("cover.png");
        image::DynamicImage::new_rgb8(2400,1800).save(&path).unwrap();
        let mut s=loaded_state(&path);
        assert_eq!(s.settings.format,Format::Same);
        s.settings.mode=Mode::Size;s.settings.ratio=1;s.settings.portrait=true;s.settings.format=Format::Jpeg;
        for slug in ["tokyo-night","tokyo-night-day"] {
            let theme=crate::tui::theme::theme_by_slug_or_default(slug);
            let mut term=Terminal::new(TestBackend::new(80,24)).unwrap();
            term.draw(|f|draw(f,&mut s,theme)).unwrap();
            let area=s.area;
            let row=|x:u16,y:u16,w:u16|(x..x+w).map(|x|term.backend().buffer().get(area.x+x,area.y+y).symbol()).collect::<String>();
            assert_eq!(row(16,7,19),"W  1080  × H  1352 ");
            assert_eq!(row(51,13,9),"◂   90  ▸");
            assert_eq!(row(58,10,14)," ☑  No upscale");
            assert_eq!(s.numeric_text(Target::H),"1352");
            s.popup_open(Popup::Transform,Target::Rotate);
            term.draw(|f|draw(f,&mut s,theme)).unwrap();
            let start=s.popup_area.right();
            let text=(start..start+11).map(|x|term.backend().buffer().get(x,area.y+10).symbol()).collect::<String>();
            assert_eq!(text," No upscale");
            s.close_popup();
        }
    }
    #[test]
    fn custom_edit_cancel_and_invalid_submission_restore_preset() {
        let d=tempfile::tempdir().unwrap();let path=d.path().join("cover.png");image::DynamicImage::new_rgb8(1200,800).save(&path).unwrap();let app=AppState::new_for_test(crate::config::TonepoetConfig::default());
        for slug in ["tokyo-night","tokyo-night-day"]{let theme=crate::tui::theme::theme_by_slug_or_default(slug);let mut term=Terminal::new(TestBackend::new(80,24)).unwrap();let mut s=loaded_state(&path);
            term.draw(|f|draw(f,&mut s,theme)).unwrap();click_target(&app,&mut s,Target::Preset(0));
            for scenario in 0..4{term.draw(|f|draw(f,&mut s,theme)).unwrap();if scenario==1{s.focus=Target::Preset(0);press(&app,&mut s,KeyCode::Char('9'));}else{click_target(&app,&mut s,Target::Custom);}
                assert_eq!(s.presets[2],4);assert_eq!(s.settings.long,1080);term.draw(|f|draw(f,&mut s,theme)).unwrap();let radio=s.hits.iter().find(|h|h.target==Target::Preset(4)).unwrap();assert_eq!(term.backend().buffer().get(s.area.x+radio.x,s.area.y+radio.y).symbol(),"●");
                match scenario{0|1=>press(&app,&mut s,KeyCode::Esc),2=>{press(&app,&mut s,KeyCode::Char('.'));press(&app,&mut s,KeyCode::Enter);},_=>{press(&app,&mut s,KeyCode::Backspace);press(&app,&mut s,KeyCode::Tab);}}
                assert!(s.edit.is_none());assert_eq!(s.presets[2],0);assert_eq!(s.customs[2],1080.);assert_eq!(s.settings.long,480);assert_eq!(s.geometry().unwrap().target,(480,320));assert!(summary(&s).1.contains("480×320"));term.draw(|f|draw(f,&mut s,theme)).unwrap();let radio=s.hits.iter().find(|h|h.target==Target::Preset(0)).unwrap();assert_eq!(term.backend().buffer().get(s.area.x+radio.x,s.area.y+radio.y).symbol(),"●");
            }
            click_target(&app,&mut s,Target::Custom);for c in "720".chars(){press(&app,&mut s,KeyCode::Char(c));}press(&app,&mut s,KeyCode::Enter);assert_eq!(s.settings.long,720);assert_eq!(s.presets[2],4);term.draw(|f|draw(f,&mut s,theme)).unwrap();click_target(&app,&mut s,Target::Preset(0));term.draw(|f|draw(f,&mut s,theme)).unwrap();click_target(&app,&mut s,Target::Preset(4));assert_eq!(s.settings.long,720);assert!(s.edit.is_none());
        }
    }
    #[test]
    fn automatic_pad_popup_is_a_noop_and_explicit_alpha_survives() {
        let d=tempfile::tempdir().unwrap();let path=d.path().join("source.png");image::DynamicImage::new_rgb8(32,16).save(&path).unwrap();let app=AppState::new_for_test(crate::config::TonepoetConfig::default());
        for square in [true,false]{for format in [Format::Png,Format::Webp,Format::Jpeg]{let mut s=loaded_state(&path);let theme=app.theme;let mut term=Terminal::new(TestBackend::new(80,24)).unwrap();s.settings.format=format;
            if square{s.settings.mode=Mode::Off;s.settings.square=Square::Pad;}else{s.settings.mode=Mode::Size;s.settings.ratio=8;s.settings.width=32;s.settings.height=32;s.settings.fill=Fill::Pad;}
            let before=pipe::encode(s.current().unwrap(),&s.settings).unwrap();let open=|s:&mut EditImageState,term:&mut Terminal<TestBackend>|{if square&&s.popup.is_none(){term.draw(|f|draw(f,s,theme)).unwrap();click_target(&app,s,Target::Transform);}term.draw(|f|draw(f,s,theme)).unwrap();click_target(&app,s,Target::Pad);term.draw(|f|draw(f,s,theme)).unwrap();};
            open(&mut s,&mut term);assert_eq!(s.color_value(),Some(if format.alpha(){[0,0,0,0]}else{[0,0,0,255]}));let revision=s.revision;let b=term.backend().buffer();let c1=b.get(s.popup_area.x+32,s.popup_area.y+1).fg;let c2=b.get(s.popup_area.x+33,s.popup_area.y+1).fg;assert_eq!(c1==c2,!format.alpha());click_target(&app,&mut s,Target::ColorDone);assert!(s.settings.pad.is_none());assert_eq!(s.revision,revision);assert_eq!(pipe::encode(s.current().unwrap(),&s.settings).unwrap().bytes,before.bytes);
            let alpha=|encoded:&Encoded|image::load_from_memory(&encoded.bytes).unwrap().to_rgba8().get_pixel(0,0)[3];assert_eq!(alpha(&before),if format.alpha(){0}else{255});
            open(&mut s,&mut term);for c in "00000080".chars(){press(&app,&mut s,KeyCode::Char(c));}press(&app,&mut s,KeyCode::Enter);assert_eq!(s.settings.pad,Some([0,0,0,128]));let explicit=pipe::encode(s.current().unwrap(),&s.settings).unwrap();assert_eq!(alpha(&explicit),if format.alpha(){128}else{255});
            open(&mut s,&mut term);for c in "FF0000FF".chars(){press(&app,&mut s,KeyCode::Char(c));}press(&app,&mut s,KeyCode::Esc);assert_eq!(s.settings.pad,Some([0,0,0,128]));assert_eq!(pipe::encode(s.current().unwrap(),&s.settings).unwrap().bytes,explicit.bytes);
            open(&mut s,&mut term);click_target(&app,&mut s,Target::ColorDefault);assert!(s.settings.pad.is_none());assert_eq!(pipe::encode(s.current().unwrap(),&s.settings).unwrap().bytes,before.bytes);
        }}
    }
    #[test]
    fn split_volume_scratch_falls_back_for_image_and_file_plus_reembed() {
        use lofty::{file::{AudioFile,TaggedFileExt}, picture::{MimeType,Picture,PictureType}};
        use std::{collections::BTreeSet,fs};
        let _coordination=crate::concurrency::isolated_test_coordination_root();
        let a=tempfile::tempdir().unwrap();
        // The normal Linux host has a separate tmpfs at /dev/shm. Other
        // hosts can supply a writable root on a second real filesystem.
        let requested=std::env::var_os("TONEPOET_TEST_OTHER_VOLUME");
        let explicit=requested.is_some();
        let root=requested.map(PathBuf::from).or_else(||cfg!(target_os="linux").then(||PathBuf::from("/dev/shm")));
        let Some(root)=root else {eprintln!("split-volume fixture skipped: set TONEPOET_TEST_OTHER_VOLUME");return;};
        let b=match tempfile::tempdir_in(&root) {
            Ok(dir)=>dir,
            Err(error) if !explicit=>{eprintln!("split-volume fixture skipped: {error}");return;},
            Err(error)=>panic!("cannot create fixture on requested second volume: {error}"),
        };
        if super::super::disc_browser::same_filesystem_paths(a.path(),b.path()) {
            assert!(!explicit,"TONEPOET_TEST_OTHER_VOLUME must be on a different filesystem from the test temp directory");
            eprintln!("split-volume fixture skipped: default paths are on the same filesystem");return;
        }
        let album=a.path().join("album");let cache=a.path().join("private-cache");
        let output=b.path().join("output");let scratch=b.path().join("scratch");
        for path in [&album,&output,&scratch]{fs::create_dir(path).unwrap();}
        let names=|path:&Path|->BTreeSet<std::ffi::OsString>{fs::read_dir(path).unwrap().map(|e|e.unwrap().file_name()).collect()};
        let source_file=album.join("original.png");let ordinary_output=album.join("edited.png");
        image::DynamicImage::new_rgb8(24,16).save(&source_file).unwrap();
        fs::write(album.join(".keep"),b"album keep").unwrap();fs::write(output.join(".keep"),b"output keep").unwrap();
        let initial_album=names(&album);let initial_output=names(&output);
        pipe::with_test_stage_roots(vec![cache.clone()],||{
            let ordinary_bytes=fs::read(&source_file).unwrap();
            let observe=||{assert_eq!(names(&album),initial_album);assert_eq!(names(&output),initial_output);Ok(())};
            assert!(pipe::write_image_with_commit(&ordinary_output,&ordinary_bytes,false,Some(&source_file),Some(&scratch),&observe,&observe).unwrap());
            assert_eq!(fs::read(&ordinary_output).unwrap(),ordinary_bytes);
            assert!(cache.is_dir());assert!(!scratch.join("tonepoet-edit-image").exists());
            let mut expected_album=initial_album.clone();expected_album.insert(ordinary_output.file_name().unwrap().to_owned());
            assert_eq!(names(&album),expected_album);assert_eq!(names(&output),initial_output);

            let audio=album.join("album.flac");
            fs::write(&audio,include_bytes!("../../tests/fixtures/browse_artwork/embedded_valid.flac")).unwrap();
            // Exercise a non-selected picture as well as the fixture's
            // existing Vorbis comment and lossless audio payload.
            let mut tagged=lofty::read_from_path(&audio).unwrap();
            let tag=tagged.primary_tag_mut().unwrap();let front=tag.pictures()[0].clone();
            tag.push_picture(Picture::new_unchecked(PictureType::CoverBack,Some(MimeType::Png),Some("Unrelated back cover".into()),front.data().to_vec()));
            tagged.save_to_path(&audio,lofty::config::WriteOptions::default()).unwrap();
            let before=lofty::read_from_path(&audio).unwrap();
            let original_audio=fs::read(&audio).unwrap();
            let payload_start=|bytes:&[u8]|->usize{
                assert_eq!(&bytes[..4],b"fLaC");let mut pos=4;
                loop{let last=bytes[pos]&0x80!=0;let len=u32::from_be_bytes([0,bytes[pos+1],bytes[pos+2],bytes[pos+3]]) as usize;pos+=4+len;if last{return pos;}}
            };
            let (pictures,index)=pipe::pictures(&audio).unwrap();let source=pictures[index].clone();
            assert!(source.can_reembed());let input=pipe::decode(&source).unwrap();
            let mut settings=Settings::new(true);settings.mode=Mode::Off;
            settings.format=if input.format==Format::Png{Format::Jpeg}else{Format::Png};
            settings.write=WriteMode::Both;settings.working_root=Some(scratch.clone());
            let image=output.join(format!("cover.{}",settings.format.extension()));settings.template=image.to_string_lossy().into_owned();
            let encoded=pipe::encode(&input,&settings).unwrap();
            let (tx,_events)=mpsc::channel();let (_answer,answers)=mpsc::channel();
            let flags=(Arc::new(CancelFlag::new()),Arc::new(AtomicBool::new(false)),SkipGate::default());
            let mut totals=ProgressTotals::default();let mut effects=JobEffects::default();
            let status=run_job(&[source],0,&input,&encoded,&settings,&tx,&answers,&flags,&mut totals,&mut effects).unwrap();
            assert!(status.contains("artwork re-embedded"));assert_eq!((effects.files_written,effects.tracks_updated),(1,1));
            assert_eq!((totals.completed,totals.errors),(1,0));assert_eq!(fs::read(&image).unwrap(),*encoded.bytes);
            expected_album.insert(audio.file_name().unwrap().to_owned());let mut expected_output=initial_output.clone();expected_output.insert(image.file_name().unwrap().to_owned());
            assert_eq!(names(&album),expected_album);assert_eq!(names(&output),expected_output);
            assert!(names(&cache).is_empty());assert!(names(&scratch.join("tonepoet-edit-image")).is_empty());
            let changed=fs::read(&audio).unwrap();assert_eq!(&original_audio[payload_start(&original_audio)..],&changed[payload_start(&changed)..]);
            let after=lofty::read_from_path(&audio).unwrap();
            let old_tag=before.primary_tag().unwrap();let new_tag=after.primary_tag().unwrap();
            assert!(old_tag.items().into_iter().eq(new_tag.items().into_iter()));
            assert_eq!(old_tag.pictures().len(),new_tag.pictures().len());
            let back=|pictures:&[Picture]|pictures.iter().position(|p|p.pic_type()==PictureType::CoverBack).unwrap();
            let old_back=&old_tag.pictures()[back(old_tag.pictures())];let new_back=&new_tag.pictures()[back(new_tag.pictures())];
            assert_eq!(old_back.data(),new_back.data());assert_eq!(old_back.description(),new_back.description());assert_eq!(old_back.mime_type(),new_back.mime_type());
            let (after_pictures,index)=pipe::pictures(&audio).unwrap();assert_eq!(after_pictures[index].bytes().unwrap(),*encoded.bytes);
        });
    }

    #[test]
    fn published_file_is_reported_after_reembed_failure_or_abort() {
        let _coordination=crate::concurrency::isolated_test_coordination_root();
        for aborted in [false,true]{let d=tempfile::tempdir().unwrap();let audio=d.path().join("album.flac");let original=include_bytes!("../../tests/fixtures/browse_artwork/embedded_valid.flac");std::fs::write(&audio,original).unwrap();let (pictures,index)=pipe::pictures(&audio).unwrap();let source=pictures[index].clone();assert!(source.can_reembed());let input=pipe::decode(&source).unwrap();let mut settings=Settings::new(true);settings.mode=Mode::Off;settings.format=Format::Png;settings.write=WriteMode::Both;settings.template="%DIR%/%NAME%-edited.%EXT%".into();let encoded=pipe::encode(&input,&settings).unwrap();let path=d.path().join("album-edited.png");
            let later=d.path().join("later.png");image::DynamicImage::new_rgb8(8,8).save(&later).unwrap();let sources=vec![source,Source::File(later.clone())];let (tx,events)=mpsc::channel();let (_answer,answers)=mpsc::channel();let flags=(Arc::new(CancelFlag::new()),Arc::new(AtomicBool::new(false)),SkipGate::default());let mut totals=ProgressTotals::default();let mut effects=JobEffects::default();
            let result=run_job_with_reembed(&sources,0,&input,&encoded,&settings,&tx,&answers,&flags,&mut totals,&mut effects,&|_| {},&|source,e,other,check,cancel|{assert_eq!(std::fs::read(&path).unwrap(),*encoded.bytes);if aborted{cancel.cancel();pipe::reembed_checked(source,e,other,None,&||check(),cancel)}else{Err("injected native writer failure before mutation".into())}}).unwrap_err();
            assert_eq!(result.aborted,aborted);assert!(result.status.contains(&path.display().to_string()));assert!(result.status.contains(if aborted{"Re-embed cancelled"}else{"Re-embed failed"}));assert_eq!((totals.items_done,totals.completed,totals.errors,totals.skipped,totals.not_attempted),(1,0,1,0,1));assert_eq!((effects.files_written,effects.tracks_updated),(1,0));assert!(effects.changed());assert_eq!(std::fs::read(&path).unwrap(),*encoded.bytes);assert_eq!(std::fs::read(&audio).unwrap().as_slice(),original.as_slice());assert!(!d.path().join("later-edited.png").exists());assert_eq!(std::fs::read_dir(d.path()).unwrap().count(),3);
            assert!(events.try_iter().any(|event|matches!(event,JobEvent::Update(FileTaskProgressUpdate::RecordError{error,..}) if error.destination==Some(path.clone()))));
        }
    }
    #[test]
    fn dirty_cancels_old_worker_and_starts_only_latest_revision() {
        let d=tempfile::tempdir().unwrap();let path=d.path().join("source.png");image::DynamicImage::new_rgb8(64,64).save(&path).unwrap();let mut s=loaded_state(&path);let theme=crate::tui::theme::theme_by_slug_or_default("tokyo-night");let mut picker=ratatui_image::picker::Picker::new((8,12));let source=s.source().unwrap().clone();let input=s.current().unwrap().clone();let stale=pipe::encode(&input,&s.settings).unwrap();let flag=Arc::new(AtomicBool::new(false));let (release,wait)=mpsc::channel();let (tx,rx)=mpsc::channel();let old_revision=s.revision;let pending=Pending(Arc::new(Mutex::new(rx)));s.work=Some(EncodeWorker{pending,lease:Arc::new(EncodeLease(flag.clone())),revision:old_revision});
        for long in [48,40,32]{s.settings.long=long;s.dirty();assert!(flag.load(Ordering::Acquire));s.changed=Instant::now()-Duration::from_millis(150);s.prepare(&mut picker,0,0,theme);assert_eq!(s.work.as_ref().unwrap().revision,old_revision);assert!(s.encoded.is_none());}
        // Simulate an uninterruptible codec completing despite cancellation.
        thread::spawn(move||{wait.recv().unwrap();let _=tx.send(Work{revision:old_revision,source,result:Ok((input,Ok(stale)))});});release.send(()).unwrap();let deadline=Instant::now()+Duration::from_secs(5);while s.encoded.is_none()&&Instant::now()<deadline{s.prepare(&mut picker,0,0,theme);thread::sleep(Duration::from_millis(1));}assert!(s.work.is_none());assert_eq!(s.encoded.as_ref().unwrap().geometry.target,(32,32));assert!(s.problem.is_none());
    }

}
