from pyroute2 import NetNS
from pyroute2 import IPDB
from pyroute2 import NSPopen

host1_ns = NetNS('host1')
ipdb_host1 = IPDB(nl=host1_ns)

host2_ns = NetNS('host2')
ipdb_host2 = IPDB(nl=host2_ns)

ipdb_host1.create(ifname='v0p0', kind='veth', peer='v0p1').commit()

with ipdb_host1.interfaces.v0p1 as veth:
    veth.net_ns_fd = 'host2' # Attach to host 2


with ipdb_host1.interfaces.v0p0 as veth:
    veth.add_ip('172.16.200.1/24')
    veth.up()

with ipdb_host2.interfaces.v0p1 as veth:
    veth.add_ip('172.16.200.2/24')
    veth.up()

consoles = []

consoles.append(NSPopen('host1', ['konsole', '-p', "tabtitle='host1'"]))
consoles.append(NSPopen('host2', ['konsole', '-p', "tabtitle='host2'"]))

for c in consoles:
    c.wait()
    c.release()

ipdb_host1.release()
host1_ns.close()
host1_ns.remove()

ipdb_host2.release()
host2_ns.close()
host2_ns.remove()
