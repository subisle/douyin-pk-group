
#!/usr/bin/env python3
import subprocess, sys, json
from datetime import date, datetime

def norm_null(rows):
    return [[None if x=="NULL" else x for x in row] for row in rows]

def q_mysql(sql):
    out = subprocess.check_output(["mysql","-h127.0.0.1","-uroot","-NBe",sql,"douyin_dev"], text=True)
    return norm_null([line.split("\t") for line in out.splitlines()]) if out.strip() else []
def q_remote(sql):
    cmd = ["ssh","-o","BatchMode=yes","root@192.168.5.12",
           f'docker exec douyin-pk-suite-mariadb-1 mariadb -uroot -p"Mr2026#rk3318" -NBe "{sql}" douyin']
    out = subprocess.check_output(cmd, text=True)
    return norm_null([line.split("\t") for line in out.splitlines()]) if out.strip() else []
def q_pg(sql):
    out = subprocess.check_output(["/opt/homebrew/opt/postgresql@17/bin/psql","-p","3318","-h","127.0.0.1",
        "-U","douyin_dev","-d","douyin_dev","-At","-F","\t","-c","SET search_path TO public; "+sql], text=True)
    lines = out.splitlines()
    if lines and lines[0] == "SET": lines = lines[1:]
    return norm_null([line.split("\t") for line in lines if line.strip()])
def q_pg_one(sql):
    return q_pg(sql)[0][0]

def esc(v):
    if isinstance(v,bool): return "true" if v else "false"
    if v is None or str(v) == "NULL": return "NULL"
    v = str(v).replace("'","''")
    return "'"+v+"'"

def insert(table, cols, rows, conflict=None):
    if rows and not isinstance(rows[0], (list, tuple)):
        rows = [rows]
    rows = [r for r in rows if r is not None]
    if not rows: return
    vals = []
    for row in rows:
        if isinstance(row, bool):
            continue
        vals.append("("+",".join(esc(x) for x in row)+")")
    sql = f"INSERT INTO public.{table} ({','.join(cols)}) VALUES {','.join(vals)} " + (f"ON CONFLICT ({conflict}) DO NOTHING" if conflict else "ON CONFLICT DO NOTHING")
    subprocess.check_call(["/opt/homebrew/opt/postgresql@17/bin/psql","-p","3318","-h","127.0.0.1",
        "-U","douyin_dev","-d","douyin_dev","-v","ON_ERROR_STOP=1","-q","-c","SET search_path TO public; "+sql])

# Build map: anchor_id -> person_id
print("== person ==")
subprocess.check_call(["/opt/homebrew/opt/postgresql@17/bin/psql","-p","3318","-h","127.0.0.1","-U","douyin_dev","-d","douyin_dev","-v","ON_ERROR_STOP=1","-q","-c","TRUNCATE public.person,public.account,public.daily_metric,public.import_batch,public.wave_snapshot,public.my_accounts,public.my_targets,public.wx_push_accounts RESTART IDENTITY CASCADE"])
local_person = q_mysql("SELECT id,name,gender,master_id,generation,group_name,avatar_url,hide_in_daily_report,status,joined_at FROM person")
remote_person = q_remote("SELECT id,name,gender,master_id,generation,group_name,avatar_url,hide_in_daily_report,status,joined_at FROM person")

# First insert all local person with new ids
seen=set()
for r in local_person:
    if r[1] in seen:
        continue
    seen.add(r[1])
    insert("person", ["name","gender","master_id","generation","group_name","avatar_url","hide_in_daily_report","status","joined_at"], [r[1],r[2],None,r[4],r[5],r[6],r[7]=="1",r[8],r[9]], conflict="name")

# Build local name -> new pg id
name_map = {}
for row in q_pg("SELECT name, id FROM person"):
    name_map[row[0]] = row[1]

# Insert remote person only if not exists by name
new_remote = []
for r in remote_person:
    if r[1] not in name_map:
        new_remote.append(r)
for r in new_remote:
    if r[1] in seen:
        continue
    seen.add(r[1])
    insert("person", ["name","gender","master_id","generation","group_name","avatar_url","hide_in_daily_report","status","joined_at"], [r[1],r[2],None,r[4],r[5],r[6],r[7]=="1",r[8],r[9]], conflict="name")

# Rebuild name_map
name_map = {}
for row in q_pg("SELECT name, id FROM person"):
    name_map[row[0]] = row[1]
print("person pg:", len(name_map))

# Build map: anchor_id -> pg_person_id via local account
print("== account ==")
local_account = q_mysql("SELECT id,person_id,anchor_id,douyin_no,anchor_name,is_primary,status FROM account")
remote_account = q_remote("SELECT id,person_id,anchor_id,douyin_no,anchor_name,is_primary,status FROM account")

for r in local_account:
    local_person_id = r[1]
    # map via original person name
    name = next((x[1] for x in local_person if x[0]==local_person_id), None)
    pg_person_id = name_map.get(name)
    if pg_person_id:
        douyin_no = r[3] if r[3] not in (None,"NULL") else ""
        insert("account", ["person_id","anchor_id","douyin_no","anchor_name","is_primary","status"], [pg_person_id,r[2],douyin_no or "",r[4],r[5]=="1",r[6]], conflict="anchor_id")

for r in remote_account:
    local_person_id = r[1]
    name = next((x[1] for x in remote_person if x[0]==local_person_id), None)
    pg_person_id = name_map.get(name)
    if pg_person_id:
        douyin_no = r[3] if r[3] not in (None,"NULL") else ""
        insert("account", ["person_id","anchor_id","douyin_no","anchor_name","is_primary","status"], [pg_person_id,r[2],douyin_no or "",r[4],r[5]=="1",r[6]], conflict="anchor_id")

# Build anchor -> pg_person_id
anchor_map = {}
for row in q_pg("SELECT anchor_id, person_id FROM account"):
    anchor_map[row[0]] = row[1]

print("== daily_metric ==")
# Import local first (priority), then remote (skip if key exists)
local_daily = q_mysql("SELECT person_id,anchor_id,biz_date,wave,cumulative_wave,prev_snapshot_date,wave_span,wave_reliable,minutes,cumulative_minutes,minutes_span,minutes_reliable,is_live,tier,source_batch_id FROM daily_metric")
remote_daily = q_remote("SELECT person_id,anchor_id,biz_date,wave,cumulative_wave,prev_snapshot_date,wave_span,wave_reliable,minutes,cumulative_minutes,minutes_span,minutes_reliable,is_live,tier,source_batch_id FROM daily_metric")

def daily_row(r, person_lookup, batch_map):
    pid = person_lookup.get(r[0])
    aid = r[1]
    if pid is None:
        return None
    if aid in anchor_map and anchor_map[aid] != pid:
        pid = anchor_map[aid]
    return [pid, aid, r[2], r[3], r[4], r[5], r[6], r[7], r[8], r[9], r[10], r[11], r[12], r[13], batch_map.get(r[14])]

# import batches first
print("== import_batch ==")
try:
    local_batch = [r+[""]*(9-len(r)) for r in q_mysql("SELECT id,source,filename,biz_date,team,imported_rows,ok_rows,failed_rows,note FROM import_batch")]
except Exception:
    local_batch = [[r[0],r[1],r[2],None,None,r[3],None,None,None] for r in q_mysql("SELECT id,source,filename,imported_rows FROM import_batch")]
remote_batch = [[r[0],r[1],r[2],None,None,r[3],None,None,None] for r in q_remote("SELECT id,source,filename,imported_rows FROM import_batch") if len(r)>=4]
batch_map = {}
for r in local_batch:
    if not any(r[1:9]):
        continue
    insert("import_batch", ["source","filename","biz_date","team","imported_rows","ok_rows","failed_rows","note"], [r[1],r[2],r[3],r[4],r[5],r[6],r[7],r[8]])
for r in remote_batch:
    if not any(r[1:9]):
        continue
    insert("import_batch", ["source","filename","biz_date","team","imported_rows","ok_rows","failed_rows","note"], [r[1],r[2],r[3],r[4],r[5],r[6],r[7],r[8]])

# build old batch id -> new batch id by matching filename+biz_date+source
for old, r in [( ("local",r[0]), r) for r in local_batch] + [(("remote",r[0]), r) for r in remote_batch]:
    src, fname, bd = r[1], r[2], r[3]
    match = q_pg(f"SELECT id FROM import_batch WHERE source={esc(src)} AND filename={esc(fname)} AND COALESCE(biz_date::text,'')={esc(bd or '')} ORDER BY id LIMIT 1")
    if match: batch_map[old] = match[0][0]

local_person_lookup = {r[0]: name_map.get(r[1]) for r in local_person}
remote_person_lookup = {r[0]: name_map.get(r[1]) for r in remote_person}

rows = []
for r in local_daily:
    x = daily_row(r, local_person_lookup, batch_map)
    if x: rows.append(x)
print("local daily rows:", len(rows))
# insert in chunks
for i in range(0,len(rows),500):
    insert("daily_metric", ["person_id","anchor_id","biz_date","wave","cumulative_wave","prev_snapshot_date","wave_span","wave_reliable","minutes","cumulative_minutes","minutes_span","minutes_reliable","is_live","tier","source_batch_id"], rows[i:i+500], conflict="person_id,biz_date")

rows = []
for r in remote_daily:
    x = daily_row(r, remote_person_lookup, batch_map)
    if x: rows.append(x)
print("remote daily rows:", len(rows))
for i in range(0,len(rows),500):
    insert("daily_metric", ["person_id","anchor_id","biz_date","wave","cumulative_wave","prev_snapshot_date","wave_span","wave_reliable","minutes","cumulative_minutes","minutes_span","minutes_reliable","is_live","tier","source_batch_id"], rows[i:i+500], conflict="person_id,biz_date")

print("== wave_snapshot ==")
local_snap = q_mysql("SELECT anchor_id,biz_date,wave_value,rank_in_guild,source_batch_id FROM wave_snapshot")
remote_snap = q_remote("SELECT anchor_id,biz_date,wave_value,rank_in_guild,source_batch_id FROM wave_snapshot")
for r in local_snap+remote_snap:
    insert("wave_snapshot", ["anchor_id","biz_date","wave_value","rank_in_guild","source_batch_id"], [r[0],r[1],r[2],r[3],batch_map.get(r[4])], conflict="anchor_id,biz_date")

print("== my_accounts ==")
local_my = q_mysql("SELECT name,phone,pass,is_admin,created_at,bound,bound_at,source,wb_state FROM my_accounts")
remote_my = q_remote("SELECT name,phone,pass,is_admin,created_at,bound,bound_at,source,wb_state FROM my_accounts")
for r in local_my+remote_my:
    insert("my_accounts", ["name","phone","pass","is_admin","created_at","bound","bound_at","source","wb_state"], [r[0],r[1],r[2],r[3],r[4],r[5],r[6],r[7],r[8]])

print("== my_targets ==")
local_targets = q_mysql("SELECT name,team,kind,target_person_id,target_name,target_value,hit,last_gap,last_notice_date,created_at FROM my_targets")
remote_targets = q_remote("SELECT name,team,kind,target_person_id,target_name,target_value,hit,last_gap,last_notice_date,created_at FROM my_targets")
for r in local_targets+remote_targets:
    insert("my_targets", ["name","team","kind","target_person_id","target_name","target_value","hit","last_gap","last_notice_date","created_at"], [r[0],r[1],r[2],r[3],r[4],r[5],r[6],r[7],r[8],r[9]])

print("== wx_push_accounts ==")
local_wx = q_mysql("SELECT name,team,person_id,ilink_bot_id,ilink_user_id,bot_token,base_url,nickname,conversation_id,updates_buf,bound_at,updated_at FROM wx_push_accounts")
remote_wx = q_remote("SELECT name,team,person_id,ilink_bot_id,ilink_user_id,bot_token,base_url,nickname,conversation_id,updates_buf,bound_at,updated_at FROM wx_push_accounts")
for r in local_wx+remote_wx:
    insert("wx_push_accounts", ["name","team","person_id","ilink_bot_id","ilink_user_id","bot_token","base_url","nickname","conversation_id","updates_buf","bound_at","updated_at"], [r[0],r[1],r[2],r[3],r[4],r[5],r[6],r[7],r[8],r[9],r[10],r[11]])

print("== done ==")
for t in ["person","account","daily_metric","import_batch","wave_snapshot","my_accounts","my_targets","wx_push_accounts"]:
    print(t, q_pg_one(f"SELECT COUNT(*) FROM {t}"))
