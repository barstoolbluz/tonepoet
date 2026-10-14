"""Execute the actual store_analysis SQL from src/db.rs against SQLite."""
from pathlib import Path
import re
import sqlite3

source = Path(__file__).resolve().parents[1] / 'src/db.rs'
text = source.read_text()
section = text.split('pub fn store_analysis(', 1)[1].split('// ── Conversion queue', 1)[0]
match = re.search(r'"(INSERT OR REPLACE INTO analysis_cache \(.*?\))"\s*,\s*params!\[', section, re.S)
assert match, 'could not recover store_analysis SQL'
sql = match.group(1)
placeholders = {int(n) for n in re.findall(r'\?(\d+)', sql)}
assert placeholders == set(range(1,23)), placeholders
columns = ['file_path TEXT PRIMARY KEY', 'file_mtime INTEGER','file_size INTEGER','algo_version INTEGER',
           'dr_value INTEGER','peak_db REAL','rms_db REAL','clipping_count INTEGER','dc_bias REAL',
           'actual_bit_depth INTEGER','declared_bit_depth INTEGER','sample_rate INTEGER','channels INTEGER',
           'duration_secs REAL','lufs REAL','true_peak_dbtp REAL','preemphasis INTEGER','preemphasis_corr REAL',
           'preemphasis_detail TEXT','hdcd_detected INTEGER','hdcd_detail TEXT',
           'hdcd_capabilities TEXT','hdcd_mtime_ns INTEGER','analyzed_at TEXT']
conn = sqlite3.connect(':memory:')
conn.execute(f'CREATE TABLE analysis_cache ({", ".join(columns)})')
base = ['/music/album.flac',1000,1024,28,7,-0.2,-11.5,0,0,16,16,44100,2,20.0,-14.0,-.1,0,None,None,1,'HDCD', 'now']
conn.execute('INSERT INTO analysis_cache(file_path,file_mtime,file_size,algo_version,hdcd_capabilities,hdcd_mtime_ns) VALUES (?1,?2,?3,?4,?5,?6)', (base[0],base[1],base[2],base[3],'{"detected":true}',1000100))
fetch=lambda: conn.execute('SELECT hdcd_capabilities,hdcd_mtime_ns FROM analysis_cache').fetchone()
conn.execute(sql, base)
assert fetch() == ('{"detected":true}',1000100), fetch()
base[3] = 29
conn.execute(sql,base)
assert fetch() == (None,None),fetch()
conn.execute('UPDATE analysis_cache SET hdcd_capabilities=?,hdcd_mtime_ns=?',('{"detected":false}',1000100))
base[1] = 1001
conn.execute(sql,base)
assert fetch() == (None,None),fetch()
print('PASS: actual store_analysis SQLite SQL preserves matching HDCD data and clears on version or identity changes')
