#!/usr/bin/env python3
"""Test one IPBAN config in an isolated network; never change the host firewall.
Usage (Linux, root): python3 tests/firewall_acceptance.py CONFIG
Requires nft, ip and unshare. The config must have at least two blocked IPv4s.
"""
import ipaddress,json,os,pathlib,re,socket,subprocess,sys

def run(*args,ok=True):
    p=subprocess.run(list(map(str,args)),capture_output=True,text=True,timeout=15)
    if ok and p.returncode:raise AssertionError(p.stderr)
    return p

def main(config):
    if os.geteuid()!=0 or sys.platform!='linux':raise RuntimeError('requires Linux root')
    if os.readlink('/proc/self/ns/net')==os.readlink('/proc/1/ns/net'):
        raise RuntimeError('refuse the host network namespace')
    text=config.read_text()
    block=re.search(r'set blocked_v4\s*\{(.*?)\}',text,re.S).group(1)
    banned=list(dict.fromkeys(re.findall(r'\b(?:\d{1,3}\.){3}\d{1,3}\b',block)))
    assert len(banned)>=2
    for ip in banned:ipaddress.IPv4Address(ip)
    port=int(re.search(r'define minecraft_port\s*=\s*(\d+)',text).group(1))
    server='198.18.0.1';control='198.18.0.2'
    assert server not in banned and control not in banned
    run('ip','link','set','lo','up')
    for ip in [server,control,*banned]:run('ip','addr','add',ip+'/32','dev','lo')
    # Simulate UFW's established acceptance at filter priority 0. The IPBAN must
    # run first even for flows created before the new rules were installed.
    baseline='''table inet acceptance {
 chain input {
  type filter hook input priority 0; policy accept;
  ct state established,related accept
  tcp dport 2525 drop
 }
 chain output {
  type filter hook output priority 0; policy accept;
  ct state established,related accept
 }
}'''
    p=subprocess.run(['nft','-f','-'],input=baseline,capture_output=True,text=True)
    assert p.returncode==0,p.stderr
    listener=socket.socket();listener.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)
    listener.bind((server,port));listener.listen();listener.settimeout(.4)
    udp=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);udp.bind((server,port));udp.settimeout(.3)
    allowed_udp=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);allowed_udp.bind((control,0));allowed_udp.settimeout(.3)
    pairs=[];datagrams=[]
    def tcp_pair(ip):
        client=socket.socket();client.settimeout(.3);client.bind((ip,0));client.connect((server,port))
        accepted,_=listener.accept();accepted.settimeout(.3)
        return client,accepted
    def exchange(pair):
        client,accepted=pair;client.sendall(b'a');assert accepted.recv(1)==b'a'
        accepted.sendall(b'b');assert client.recv(1)==b'b'
    def no_data(s):
        try:s.recv(1024)
        except TimeoutError:return
        raise AssertionError('banned game traffic was delivered')
    def blocked_send(send):
        # OUTPUT DROP may reject a local UDP send immediately rather than
        # accepting it into a queue. Both outcomes must deliver no peer data.
        try:send()
        except PermissionError:pass
    try:
        for ip in banned:
            pair=tcp_pair(ip);exchange(pair);pairs.append(pair)
            client=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);client.bind((ip,0));client.settimeout(.3)
            client.sendto(b'a',(server,port));data,peer=udp.recvfrom(32);assert data==b'a'
            udp.sendto(b'b',peer);assert client.recv(1)==b'b';datagrams.append((client,peer))
        allowed=tcp_pair(control);pairs.append(allowed)
        run('nft','--check','--file',config);run('nft','--file',config)
        for pair in pairs[:-1]:
            pair[0].sendall(b'x');no_data(pair[1])
            blocked_send(lambda:pair[1].sendall(b'y'));no_data(pair[0])
        for client,peer in datagrams:
            client.sendto(b'x',(server,port));no_data(udp)
            blocked_send(lambda:udp.sendto(b'y',peer));no_data(client)
        exchange(allowed)
        allowed_udp.sendto(b'a',(server,port));data,peer=udp.recvfrom(32);assert data==b'a' and peer[0]==control
        udp.sendto(b'b',peer);assert allowed_udp.recv(1)==b'b'
        print('PASS established TCP/UDP blocked in both directions; control connection survives',flush=True)
        for ip in banned:
            with socket.socket() as client:
                client.bind((ip,0));client.settimeout(.3)
                try:client.connect((server,port))
                except TimeoutError:pass
                else:raise AssertionError('banned new TCP connection succeeded')
            with socket.socket(socket.AF_INET,socket.SOCK_DGRAM) as client:
                client.bind((ip,0));client.sendto(b'new',(server,port));no_data(udp)
        print('PASS new TCP/UDP game traffic blocked for every listed IPv4',flush=True)
        # Management and web ports must remain reachable even from a banned IP.
        for management_port in [22,80,443]:
            with socket.socket() as management:
                management.bind((server,management_port));management.listen();management.settimeout(.3)
                for ip in banned:
                    with socket.socket() as client:
                        client.bind((ip,0));client.settimeout(.3);client.connect((server,management_port))
                        accepted,_=management.accept()
                        with accepted:
                            accepted.settimeout(.3)
                            client.sendall(b'a');assert accepted.recv(1)==b'a'
                            accepted.sendall(b'b');assert client.recv(1)==b'b'
        print('PASS SSH, HTTP and HTTPS remain reachable for banned addresses',flush=True)
        # An accept policy in the earlier BAN table must not bypass a later
        # UFW deny. Netfilter still visits the later base chain.
        with socket.socket() as later_denied:
            later_denied.bind((server,2525));later_denied.listen()
            with socket.socket() as client:
                client.bind((control,0));client.settimeout(.3)
                try:client.connect((server,2525))
                except TimeoutError:pass
                else:raise AssertionError('early BAN table bypassed a later deny')
        print('PASS later firewall denies are still enforced for non-banned addresses',flush=True)
        for _ in range(2):run('nft','--file',config)
        rules=json.loads(run('nft','-j','list','table','inet','bds_ipban').stdout)['nftables']
        assert sum('chain' in e for e in rules)==2 and sum('rule' in e for e in rules)==4
        assert all(e['chain']['prio']==-10 for e in rules if 'chain' in e)
        # A failing batch must not destroy the active table that precedes it.
        invalid=text+'\nadd rule inet bds_ipban missing_chain counter drop\n'
        p=subprocess.run(['nft','-f','-'],input=invalid,capture_output=True,text=True)
        assert p.returncode
        def stable(value):
            if isinstance(value,dict):return {k:stable(v) for k,v in value.items() if k not in {'packets','bytes'}}
            if isinstance(value,list):return [stable(v) for v in value]
            return value
        assert stable(json.loads(run('nft','-j','list','table','inet','bds_ipban').stdout)['nftables'])==stable(rules)
        print('PASS atomic reapply has no duplicate chains; invalid batch preserves active rules',flush=True)
        for pair in pairs:
            for s in pair:s.close()
        pairs.clear()
        # Remove all banned entries from a private copy of this same config.
        unbanned=re.sub(r'elements\s*=\s*\{[^}]*\}', '',text,count=1)
        p=subprocess.run(['nft','-f','-'],input=unbanned,capture_output=True,text=True)
        assert p.returncode==0,p.stderr
        for ip in banned:
            pair=tcp_pair(ip);exchange(pair)
            for s in pair:s.close()
        for client,peer in datagrams:
            client.sendto(b'a',(server,port));data,source=udp.recvfrom(32);assert data==b'a' and source==peer
            udp.sendto(b'b',peer);assert client.recv(1)==b'b'
        print('PASS editing the single address set releases the listed addresses',flush=True)
    finally:
        for pair in pairs:
            for s in pair:s.close()
        for client,_ in datagrams:client.close()
        listener.close();udp.close();allowed_udp.close()

if __name__=='__main__':
    if not __debug__:raise RuntimeError('requires Python assertions enabled')
    config=pathlib.Path(sys.argv[1]).resolve()
    if '--inside' in sys.argv:main(config)
    else:
        if os.geteuid()!=0 or sys.platform!='linux':raise RuntimeError('requires Linux root')
        subprocess.run(['unshare','--net',sys.executable,__file__,str(config),'--inside'],check=True)
