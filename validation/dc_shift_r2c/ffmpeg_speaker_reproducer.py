import json,struct,subprocess,tempfile,pathlib,math,shutil
root=pathlib.Path(tempfile.mkdtemp(prefix='tonepoet-dc-r2c-'))
results=[]
for label,ch,mask,expected in [('3.0',3,'0x7','3.0'),('quad',4,'0x33','quad'),('5.1_no_center_alt',5,'0x3b',None)]:
 p=root/(label.replace('.','_')+'.f64le')
 frames=4096; offsets=[0.003, -0.002, 0.004, 0.005, -0.006][:ch]
 with p.open('wb') as f:
  for i in range(frames):
   for c in range(ch):f.write(struct.pack('<d', 0.03*math.sin(2*math.pi*i*(c+2)/159)+offsets[c]))
 dst=root/(label.replace('.','_')+'.wav'); dest=(root/(label.replace('.','_')+'.corrected.f64le'))
 with p.open('rb') as src,dest.open('wb') as out:
  data=src.read(); values=struct.iter_unpack('<d',data)
  for k,(x,) in enumerate(values): out.write(struct.pack('<d',x-offsets[k%ch]))
 cmd=['ffmpeg','-hide_banner','-loglevel','error','-y','-f','f64le','-ar','48000','-ac',str(ch),'-channel_layout',mask,'-i',str(dest),'-map','0:a:0','-c:a','pcm_f64le',str(dst)]
 subprocess.run(cmd,check=True)
 ffprobe=['ffprobe','-v','error','-select_streams','a:0','-show_entries','stream=channels,channel_layout','-of','json',str(dst)]
 observed=json.loads(subprocess.check_output(ffprobe))['streams'][0]
 # Verify actual sample identity by decoding back using output explicit map for raw.
 out=root/(label.replace('.','_')+'.roundtrip.f64le')
 subprocess.run(['ffmpeg','-v','error','-y','-i',str(dst),'-map','0:a:0','-c:a','pcm_f64le','-f','f64le',str(out)],check=True)
 assert out.read_bytes()==dest.read_bytes(),(label,'sample data changed')
 assert observed['channels']==ch,(label,observed)
 if expected is not None:assert observed.get('channel_layout')==expected,(label,observed)
 results.append({'input_layout':label,'mask':mask,'output':observed,'sample_bitwise_equal':True})
print(json.dumps(results,indent=2))
shutil.rmtree(root)
