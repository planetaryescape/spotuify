import os,json,tempfile,subprocess,pathlib,time
binary=os.environ.get('SPOTUIFY_AUDIT_BIN','/Users/bhekanik/code/bhekanik/.worktrees/spotuify-recovery-20261026/target-cli/debug/spotuify')
root=pathlib.Path(tempfile.mkdtemp(prefix='spotuify-recovery-journey-'))
env=os.environ.copy()
env.update({'SPOTUIFY_FAKE_SPOTIFY':'1','SPOTUIFY_INSTANCE':'spotuify-recovery-fake','SPOTUIFY_EXIT_WITH_PARENT':str(os.getpid()),'SPOTUIFY_CLIENT_ID':'fake-client-id'})
for key,suffix in {'RUNTIME_DIR':'runtime','SOCKET':'runtime/daemon.sock','DATA_DIR':'data','CACHE_DIR':'cache-dir','CONFIG_DIR':'config-dir','LOG_DIR':'logs','CACHE_DB':'cache.sqlite','SEARCH_INDEX':'index','ANALYTICS_DB':'analytics.sqlite','CONFIG':'spotuify.toml'}.items():env['SPOTUIFY_'+key]=str(root/suffix)
records=[]
def run(*args,json_output=True):
 p=subprocess.run([binary,*args],env=env,capture_output=True,text=True,timeout=85)
 if p.returncode:raise RuntimeError(f'{args}: exit{p.returncode}: {p.stderr} {p.stdout}')
 data=json.loads(p.stdout) if json_output else p.stdout.strip()
 records.append({'command':list(args),'result':data})
 return data
try:
 devices=run('devices','--format','json')
 assert devices
 search=run('search','luther vandross','--type','track','--format','json')
 assert search and search[0]['kind']=='track'
 track=search[0]['uri']
 play=run('play-uri',track,'--format','json');assert play['ok']
 status=run('status','--format','json');assert status['is_playing'] and status['item']['uri']==track
 playlists=run('playlists','--format','json');assert playlists
 playlist=playlists[0]['id']
 if not playlist.startswith('spotify:'):playlist='spotify:playlist:'+playlist
 tracks=run('playlist','tracks',playlist,'--format','json');assert tracks
 linked=run('playlist','tracks','https://open.spotify.com/playlist/'+playlist.split(':')[-1]+'?si=fixture','--format','json')
 assert [t['uri'] for t in tracks]==[t['uri'] for t in linked]
 selected=tracks[-1]['uri']
 context_play=run('play-uri',selected,'--context',playlist,'--format','json');assert context_play['ok']
 context_status=run('status','--format','json');assert context_status['is_playing'] and context_status['item']['uri']==selected
 if len(tracks)>1:
  run('play-uri',tracks[0]['uri'],'--context',playlist,'--format','json')
  run('next','--format','json')
  next_status=run('status','--format','json');assert next_status['item']['uri']==tracks[1]['uri']
 queue=run('queue','add',playlist,'--wait','--format','json');assert queue['ok']
 transfer=run('transfer',devices[0]['id'],'--format','json');assert transfer['ok']
 doctor=run('doctor','--format','json')
 assert doctor['daemon']['running'] and doctor['device_diagnostics'] is not None
 assert not any('runtime unavailable outside' in c['message'] for c in doctor['api_checks'])
 expected='unhealthy' if any(f['severity']=='error' for f in doctor['findings']) else 'degraded' if any(f['severity']=='warning' for f in doctor['findings']) else 'healthy'
 assert doctor['health_class']==expected
 assert doctor['healthy']==(expected=='healthy')
 records.append({'checks':{'selected_track':track,'playlist':playlist,'same_share_link_items':len(tracks),'contextual_selected_track':selected,'next_follows_collection':len(tracks)>1,'doctor_health':expected,'daemon_provider_checks':len(doctor['api_checks'])}})
finally:
 p=subprocess.run([binary,'daemon','stop'],env=env,capture_output=True,text=True,timeout=15)
 records.append({'command':['daemon','stop'],'exit':p.returncode,'stdout':p.stdout.strip(),'stderr':p.stderr.strip()})
 pathlib.Path('/Users/bhekanik/code/bhekanik/.worktrees/spotuify-recovery-20261026/docs/build-log/recovery/evidence/fake-journey-latest.json').write_text(json.dumps(records,indent=2))
print(json.dumps(records[-2:],indent=2))
