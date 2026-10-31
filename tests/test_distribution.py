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
if __name__=='__main__':unittest.main()
