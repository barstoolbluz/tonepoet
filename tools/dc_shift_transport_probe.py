import hashlib,math,os,struct,subprocess,tempfile,time
base=tempfile.mkdtemp(prefix='tonepoet-dc-transport-')
rate,channel,frames=48000,2,480000
samples=[(1.2 if (f%11)==0 else -1.13 if (f%23)==0 else math.sin(f/37.0)*0.98)+ch*.003 for f in range(frames) for ch in range(channel)]
raw=os.path.join(base,'dc-transport-in.f64le'); wav=os.path.join(base,'dc-transport.wav'); back=os.path.join(base,'dc-transport-back.f64le')
with open(raw,'wb') as out:
 for val in samples:out.write(struct.pack('<d',val))
common=['ffmpeg','-nostdin','-y','-hide_banner','-loglevel','error']
cmd1=common+['-f','f64le','-ar',str(rate),'-ac',str(channel),'-i',raw,'-map','0:a:0','-vn','-sn','-dn','-af','anull','-c:a','pcm_f64le','-f','wav',wav]
cmd2=common+['-i',wav,'-map','0:a:0','-vn','-sn','-dn','-af','anull','-c:a','pcm_f64le','-f','f64le',back]
t=time.monotonic();subprocess.run(cmd1,check=True);mid=time.monotonic();subprocess.run(cmd2,check=True);end=time.monotonic()
a=open(raw,'rb').read();b=open(back,'rb').read()
print('FFmpeg transport test version:',subprocess.check_output(['ffmpeg','-version']).decode().splitlines()[0])
print('n_frames',frames,'channels',channel,'raw_bytes',len(a),'wav_bytes',os.path.getsize(wav),'returned_bytes',len(b))
print('bit_identical',a==b,'md5-in',hashlib.md5(a).hexdigest(),'md5-out',hashlib.md5(b).hexdigest())
print('float_overload_input_max',max(samples),'output_max',max(struct.unpack_from('<d',b,i*8)[0] for i in range(len(samples))))
print('secs_f64_to_wav',round(mid-t,4),'secs_wav_to_f64',round(end-mid,4))
with open(os.path.join(base,'ffmpeg_transport_probe_result.txt'),'w') as o:
 o.write('FFmpeg 7.1.5 host (NOT pinned version) transport raw-to-WAV-to-raw\n')
 o.write(f'frames={frames} channels={channel} raw_bytes={len(a)} bit_identical={a==b} max={max(samples)}\n')
 o.write(f'secs_f64_to_wav={mid-t:.4f} secs_wav_to_f64={end-mid:.4f}\n')
