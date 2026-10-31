"""Bundle validation never requires the application source on the destination."""
import hashlib,json,os,pathlib,shutil,subprocess,tempfile,unittest
ROOT=pathlib.Path(__file__).resolve().parents[1]
COMMIT='0123456789abcdef0123456789abcdef01234567'
class Distribution(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.root=pathlib.Path(self.temp.name);self.bundle=self.root/'bundle';self.bundle.mkdir();self.bin=self.root/'commands';self.bin.mkdir()
        self.make(self.bin/'uname','case "$1" in -s) echo Linux;; -m) echo x86_64;; esac')
        if not shutil.which('sha256sum'):
            self.make(self.bin/'sha256sum','exec shasum -a 256 "$@"')
        self.make(self.bundle/'bdsctl',f'echo "bdsctl 0.1.0 ({COMMIT})"')
        (self.bundle/'BUILD.json').write_text(json.dumps({'commit':COMMIT}))
        self.manifest()
    def tearDown(self):self.temp.cleanup()
    def make(self,path,body):path.write_text('#!/bin/sh\n'+body+'\n');path.chmod(0o755)
    def manifest(self):
        (self.bundle/'SHA256SUMS').write_text(''.join(hashlib.sha256(p.read_bytes()).hexdigest()+'  '+p.name+'\n' for p in sorted(self.bundle.iterdir()) if p.name!='SHA256SUMS'))
    def validate(self,commit=COMMIT):
        return subprocess.run(['sh',str(ROOT/'deploy/install.sh'),'--check',str(self.bundle),commit],capture_output=True,text=True,errors='replace',env={**os.environ,'PATH':str(self.bin)+':'+os.environ['PATH']})
    def test_source_free_bundle_validates(self):
        result=self.validate();self.assertEqual(result.returncode,0,result.stdout+result.stderr)
        self.assertFalse((self.bundle/'src').exists())
    def test_changed_binary_rejected(self):
        with (self.bundle/'bdsctl').open('a') as f:f.write('# changed\n')
        self.assertNotEqual(self.validate().returncode,0)
    def test_wrong_commit_rejected_even_with_valid_checksum(self):
        self.assertNotEqual(self.validate('f'*40).returncode,0)
    def test_missing_manifest_rejected(self):
        (self.bundle/'SHA256SUMS').unlink();self.assertNotEqual(self.validate().returncode,0)
    def test_packaged_archive_has_no_source_and_validates(self):
        import tarfile
        repo=self.root/'clean-repo';(repo/'deploy').mkdir(parents=True)
        for name in ['package.sh','Caddyfile.example']:shutil.copy2(ROOT/'deploy'/name,repo/'deploy'/name)
        shutil.copy2(ROOT/'bds.toml.example',repo/'bds.toml.example')
        subprocess.run(['git','init','-q',str(repo)],check=True)
        for key,value in [('user.name','BDS test'),('user.email','test@example.invalid')]:subprocess.run(['git','-C',str(repo),'config',key,value],check=True)
        subprocess.run(['git','-C',str(repo),'add','.'],check=True);subprocess.run(['git','-C',str(repo),'commit','-qm','fixture'],check=True)
        commit=subprocess.check_output(['git','-C',str(repo),'rev-parse','HEAD'],text=True).strip()
        program=self.root/'compiled-bdsctl';self.make(program,f'echo "bdsctl 0.1.0 ({commit})"')
        env={**os.environ,'PATH':str(self.bin)+':'+os.environ['PATH']};env.pop('BDS_VALIDATION_REPORT',None)
        result=subprocess.run(['sh',str(repo/'deploy/package.sh'),'dist',str(program)],capture_output=True,text=True,env=env)
        self.assertEqual(result.returncode,0,result.stdout+result.stderr)
        with tarfile.open(repo/'dist/bdsctl-linux-x86_64.tar.gz') as archive:
            names={m.name.rstrip('/') for m in archive.getmembers()}
            self.assertEqual(names,{'bdsctl-linux-x86_64'}|{'bdsctl-linux-x86_64/'+n for n in ['bdsctl','BUILD.json','SHA256SUMS','bds.toml.example','Caddyfile.example']})
            target=self.root/'unpacked';archive.extractall(target)
        result=subprocess.run(['sh',str(ROOT/'deploy/install.sh'),'--check',str(target/'bdsctl-linux-x86_64'),commit],capture_output=True,text=True,env=env)
        self.assertEqual(result.returncode,0,result.stdout+result.stderr)
    def test_packager_refuses_dirty_source(self):
        repo=self.root/'repo';(repo/'deploy').mkdir(parents=True);shutil.copy2(ROOT/'deploy/package.sh',repo/'deploy/package.sh')
        subprocess.run(['git','init','-q',str(repo)],check=True)
        for key,value in [('user.name','BDS test'),('user.email','test@example.invalid')]:subprocess.run(['git','-C',str(repo),'config',key,value],check=True)
        subprocess.run(['git','-C',str(repo),'add','.'],check=True);subprocess.run(['git','-C',str(repo),'commit','-qm','fixture'],check=True)
        (repo/'dirty').write_text('uncommitted')
        result=subprocess.run(['sh',str(repo/'deploy/package.sh')],capture_output=True,text=True)
        self.assertNotEqual(result.returncode,0);self.assertIn('クリーン',result.stderr)
    def installer_fixture(self,fail=False):
        destination=self.root/'vps';destination.mkdir()
        mapping={p:str(destination/p.lstrip('/')) for p in ['/usr/local/bin','/usr/local/share/bdsctl','/etc/bds','/etc/systemd/system','/etc/polkit-1/rules.d']}
        script=(ROOT/'deploy/install.sh').read_text()
        for old,new in mapping.items():script=script.replace(old,new)
        installer=self.root/'install.sh';installer.write_text(script)
        config=pathlib.Path(mapping['/etc/bds'])/'config.toml';config.parent.mkdir(parents=True);config.write_text('fixture')
        for directory in mapping.values():pathlib.Path(directory).mkdir(parents=True,exist_ok=True)
        old_binary=pathlib.Path(mapping['/usr/local/bin'])/'bdsctl';old_binary.write_text('previous binary');old_binary.chmod(0o755)
        old_unit=pathlib.Path(mapping['/etc/systemd/system'])/'bds.service';old_unit.write_text('previous service')
        old_build=pathlib.Path(mapping['/usr/local/share/bdsctl'])/'BUILD.json';old_build.write_text('previous build')
        self.make(self.bin/'id','echo 0')
        self.make(self.bin/'systemctl','case "$*" in *LoadState*) echo not-found;; esac')
        for name in ['rsync','zstd','getent','useradd','groupadd','usermod','chown','pkcheck']:
            self.make(self.bin/name,'exit 0')
        self.make(self.bundle/'bdsctl',f'''case "$1" in
--version) echo 'bdsctl 0.1.0 ({COMMIT})';;
generate-units) mkdir -p "$2"; for name in bds.service bds-daily.timer 50-bds-discord.rules; do echo new > "$2/$name"; done;;
prepare-directories) :;;
esac''')
        if fail:self.make(self.bin/'install','case "$*" in *50-bds-discord.rules*) exit 77;; esac\nexec /usr/bin/install "$@"')
        self.manifest()
        return installer,old_binary,old_unit,old_build,mapping
    def test_preflight_does_not_install_or_stop(self):
        installer,binary,unit,build,mapping=self.installer_fixture()
        result=subprocess.run(['sh',str(installer),'--preflight',str(self.bundle),COMMIT],capture_output=True,text=True,env={**os.environ,'PATH':str(self.bin)+':'+os.environ['PATH']})
        self.assertEqual(result.returncode,0,result.stderr)
        self.assertEqual(binary.read_text(),'previous binary');self.assertEqual(unit.read_text(),'previous service')
        self.assertEqual(build.read_text(),'previous build')
    def test_preflight_refuses_symlinked_unit_without_touching_target(self):
        installer,binary,unit,build,mapping=self.installer_fixture()
        target=self.root/'foreign-service';target.write_text('untouched')
        unit.unlink();unit.symlink_to(target)
        result=subprocess.run(['sh',str(installer),'--preflight',str(self.bundle),COMMIT],capture_output=True,text=True,env={**os.environ,'PATH':str(self.bin)+':'+os.environ['PATH']})
        self.assertNotEqual(result.returncode,0);self.assertEqual(target.read_text(),'untouched')
        self.assertEqual(binary.read_text(),'previous binary')
    def test_partial_install_rolls_back_binary_units_and_metadata(self):
        installer,binary,unit,build,mapping=self.installer_fixture(fail=True)
        result=subprocess.run(['sh',str(installer),str(self.bundle),COMMIT],capture_output=True,text=True,env={**os.environ,'PATH':str(self.bin)+':'+os.environ['PATH']})
        self.assertNotEqual(result.returncode,0,result.stdout+result.stderr)
        self.assertEqual(binary.read_text(),'previous binary');self.assertEqual(unit.read_text(),'previous service')
        self.assertEqual(build.read_text(),'previous build')
        self.assertFalse((pathlib.Path(mapping['/etc/systemd/system'])/'bds-daily.timer').exists())
    def test_routine_upgrade_preserves_stopped_services_and_holds_guard_on_install_failure(self):
        for failure in [False,True]:
            with self.subTest(failure=failure):
                work=self.root/('upgrade-failure' if failure else 'upgrade-success');work.mkdir();state=work/'state';state.mkdir();log=work/'log'
                ctl=work/'bdsctl'
                self.make(ctl,f'''echo "ctl:$*" >> '{log}'
case "$1" in --version) echo previous;; maintenance-begin) touch '{state}/guard';; maintenance-end) rm '{state}/guard';; esac''')
                self.make(self.bin/'systemctl',f'''echo "systemctl:$*" >> '{log}'
case "$1:$2" in show:bds-daily.timer) echo active;; show:bds-update.timer|show:bds-discord.service) echo inactive;; show:*) echo inactive;; esac''')
                self.make(self.bin/'id','echo 0');self.make(self.bin/'flock','exit 0')
                share=work/'share';share.mkdir();(share/'BUILD.json').write_text('{}')
                bundle=work/'bundle';bundle.mkdir();shutil.copy2(ctl,bundle/'bdsctl')
                script=(ROOT/'deploy/upgrade-remote.sh').read_text().replace('/usr/local/bin/bdsctl',str(ctl)).replace('/usr/local/share/bdsctl',str(share)).replace('/var/lib/bds/state',str(state)).replace('/run/bdsctl-deploy.lock',str(work/'lock'))
                prefix=f'''install_bundle() {{
echo "install:$*" >> '{log}'
if [ "$1" = --preflight ]; then return 0; fi
return {77 if failure else 0}
}}
'''
                remote=work/'remote.sh';remote.write_text(prefix+script)
                result=subprocess.run(['sh',str(remote),str(bundle),COMMIT],capture_output=True,text=True,env={**os.environ,'PATH':str(self.bin)+':'+os.environ['PATH']})
                lines=log.read_text().splitlines()
                if failure:
                    self.assertNotEqual(result.returncode,0);self.assertTrue((state/'guard').exists());self.assertNotIn('ctl:maintenance-end',lines)
                else:
                    self.assertEqual(result.returncode,0,result.stderr);self.assertFalse((state/'guard').exists())
                    self.assertIn('systemctl:start bds-daily.timer',lines)
                    self.assertNotIn('systemctl:start bds-update.timer',lines);self.assertNotIn('systemctl:start bds-discord.service',lines)
                    self.assertLess(lines.index('systemctl:start bds-daily.timer'),lines.index('ctl:maintenance-end'))
                self.assertFalse(any('enable' in line or 'disable' in line for line in lines))

if __name__=='__main__':unittest.main()
