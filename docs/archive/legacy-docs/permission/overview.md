> 文档状态：已归档
> 警告：本文档仅保留历史参考，可能包含过时架构或旧部署方式，不可作为当前部署依据。
> 危险提示：本文档可能包含 NATS、privileged true、worker 直连 PostgreSQL/Redis、内部路径暴露等过时内容。当前实现不采用这些方案。

# OJOS Permission Core 妯″潡寮€鍙戞枃妗?
## 涓€銆佹ā鍧楀畾浣?
`Permission Core` 鏄?OJOS 鐨勫畬鏁磋祫婧愮骇鏉冮檺鏍稿績銆?
瀹冭礋璐ｈВ鍐崇殑闂鏄細

```text
璋佸彲浠ュ湪浠€涔堣祫婧愯寖鍥村唴鎵ц浠€涔堟搷浣?```

缁熶竴鎶借薄涓猴細

```text
Can(principal, permission, scope)
```

涔熷氨鏄細

```text
Can(鏉冮檺涓讳綋, 鏉冮檺鐐? 璧勬簮浣滅敤鍩?
```

绀轰緥锛?
```text
Can(user:1, "judge.submit", system:0)
Can(user:2, "problem.edit", problem:7)
Can(user:3, "problem.manage.data", problem:7)
Can(user:4, "contest.manage", contest:5)
Can(user:5, "contest.freeze", contest:5)
Can(user:6, "scoreboard.roll", contest:5)
Can(user:7, "balloon.manage", contest:5)
Can(user:8, "print.operate", contest:5)
Can(user:9, "module.install", system:0)
```

Permission Core 鐨勫畾浣嶆槸骞冲彴鍐呮牳鑳藉姏锛屼笉灞炰簬鏌愪竴涓笟鍔℃ā鍧椼€?
瀹冧笉鏄?Auth銆?
瀹冧笉鏄?Gateway銆?
瀹冧笉鏄?judge-api銆?
瀹冧笉鏄?problem-api銆?
瀹冧篃涓嶆槸 contest-api銆?
瀹冪殑鑱岃矗杈圭晫鏄細

```text
Auth
    璐熻矗鐢ㄦ埛銆佸瘑鐮併€佺櫥褰曘€丣WT銆佸熀纭€瑙掕壊璇诲彇

Gateway
    璐熻矗 JWT 楠岃瘉銆佸叆鍙ｉ壌鏉冦€佸彲淇＄敤鎴蜂笂涓嬫枃閫忎紶

Permission Core
    璐熻矗璧勬簮绾ф潈闄愬垽鏂?
涓氬姟鏈嶅姟
    璐熻矗閫夋嫨鍏蜂綋瑕佹鏌ョ殑鏉冮檺鐐瑰拰璧勬簮浣滅敤鍩?```

渚嬪锛?
```text
鐢ㄦ埛鎻愪氦浠ｇ爜
    Gateway 璐熻矗纭鐢ㄦ埛宸茬櫥褰?    judge-api 璐熻矗妫€鏌?judge.submit @ system:0
    Permission Core 璐熻矗鍒ゆ柇璇ョ敤鎴锋槸鍚︽嫢鏈夎繖涓潈闄?```

鍙堜緥濡傦細

```text
鐢ㄦ埛缂栬緫棰樼洰
    Gateway 璐熻矗纭鐢ㄦ埛宸茬櫥褰?    problem-api 璐熻矗妫€鏌?problem.edit @ problem:{id}
    Permission Core 璐熻矗鍒ゆ柇璇ョ敤鎴锋槸鍚︽嫢鏈夎繖涓潈闄?```

Permission Core 涓嶅簲璇ュ鐞嗭細

```text
鐢ㄦ埛瀵嗙爜
JWT 绛惧彂
HTTP 璺敱
鍙嶅悜浠ｇ悊
棰樼洰 CRUD
姣旇禌 CRUD
鎻愪氦鍒涘缓
鍒ら鎵ц
姒滃崟璁＄畻
妯″潡瀹夎娴佺▼
```

瀹冨彧澶勭悊鏉冮檺鍒ゆ柇鍜屾潈闄愭暟鎹淮鎶ゃ€?
---

## 浜屻€佸綋鍓嶇増鏈姸鎬?
褰撳墠 Permission Core 宸茬粡瀹屾垚鍩虹钀藉湴銆?
褰撳墠鐗堟湰鍙互璁颁负锛?
```text
Permission Core v1
```

褰撳墠宸插畬鎴愯兘鍔涳細

```text
瀹屾暣璧勬簮绾ф潈闄愭暟鎹簱妯″瀷
resource_types
permissions
role_permissions
role_bindings
permission_assignments
resource_edges
permission_audit_logs

鍏煎宸叉湁 users / roles / user_roles

shared/security/permission 鏉冮檺妫€鏌ュ櫒
HasPermission
HasUserPermission
RequireUserPermission
BindRole
AssignPermission
AddResourceEdge
RegisterResourceType
RegisterPermission
GrantRolePermission

鏀寔 system:0
鏀寔 type:0
鏀寔璧勬簮缁ф壙
鏀寔鍏ㄥ眬瑙掕壊
鏀寔璧勬簮绾ц鑹?鏀寔鐩存帴 allow
鏀寔鐩存帴 deny
鏀寔 super_admin
鏀寔杩囨湡鏃堕棿 expires_at
鏀寔鏉冮檺瀹¤鏃ュ織

judge-api 宸叉帴鍏?judge.submit
鏅€?user 瑙掕壊鍏佽鎻愪氦
permission_assignments.deny 鍙互瑕嗙洊鏅€?user 瑙掕壊鏉冮檺
鍒犻櫎 deny 鍚庢潈闄愭仮澶?```

褰撳墠宸插畬鎴愮湡瀹為獙鏀讹細

```text
permtest 鐢ㄦ埛鍙湁 user 瑙掕壊
permtest 鍙互鎻愪氦浠ｇ爜
submission 姝ｇ‘鍐欏叆 user_id
鎻愪氦鏈€缁?ACCEPTED
鍐欏叆 judge.submit @ system:0 deny 鍚庢彁浜よ forbidden 鎷︽埅
鍒犻櫎 deny 鍚庢彁浜ゆ仮澶?```

杩欒鏄庯細

```text
role_permissions 鐢熸晥
user_roles 鐢熸晥
permission_assignments.deny 鐢熸晥
deny 鍒犻櫎鍚庢潈闄愭仮澶?judge-api 宸茬湡瀹炴帴鍏?Permission Core
```

褰撳墠灏氭湭瀹屾垚鐨勭鐞嗚兘鍔涳細

```text
permission-api
鏉冮檺绠＄悊鍓嶇
瑙掕壊缁戝畾绠＄悊鎺ュ彛
鐩存帴鎺堟潈 / 鎷掔粷绠＄悊鎺ュ彛
鏉冮檺瀹¤鏃ュ織鏌ヨ鎺ュ彛
缁熶竴 JSON 閿欒鍝嶅簲
resource_edges 鑷姩缁存姢
role revoke API
permission revoke API
```

杩欎簺涓嶅奖鍝嶅綋鍓?Permission Core 鐨勬牳蹇冨垽鏂ā鍨嬨€?
---

## 涓夈€佽璁＄洰鏍?
Permission Core 鐨勬牳蹇冭璁＄洰鏍囨槸锛?*鏈潵鏂板妯″潡涓嶉渶瑕佷慨鏀规潈闄愭牳蹇冭〃缁撴瀯**銆?
涔熷氨鏄锛屽悗缁嵆浣挎柊澧烇細

```text
problem-api
contest-api
scoreboard-api
balloon-service
print-service
forum-service
clarification-service
module-registry
launcher
training-api
homework-api
course-api
virtual-contest-api
```

涔熶笉搴旇涓轰簡鏂板杩欎簺妯″潡淇敼 Permission Core 鐨勫熀纭€琛ㄧ粨鏋勩€?
鏂板妯″潡鏃讹紝鍙簲璇ュ仛锛?
```text
娉ㄥ唽 resource_type
娉ㄥ唽 permission
娉ㄥ唽 role
娉ㄥ唽 role_permissions
鍐欏叆 role_bindings
鍐欏叆 permission_assignments
鍐欏叆 resource_edges
```

渚嬪鏂板 `contest-core` 鏃讹紝鍙互娉ㄥ唽锛?
```text
resource_type:
    contest

permissions:
    contest.create
    contest.view
    contest.manage
    contest.manage.participant
    contest.manage.problem
    contest.freeze
    contest.roll
    contest.publish

roles:
    contest_owner
    contest_manager
    contest_participant
```

渚嬪鏂板 `balloon-service` 鏃讹紝鍙互娉ㄥ唽锛?
```text
resource_type:
    balloon

permissions:
    balloon.manage
    balloon.deliver

roles:
    balloon_volunteer
```

渚嬪鏂板 `launcher` 鏃讹紝鍙互娉ㄥ唽锛?
```text
resource_type:
    module

permissions:
    module.install
    module.enable
    module.disable
    module.configure
    launcher.view
    launcher.install
    launcher.uninstall
    launcher.enable
    launcher.disable
```

Permission Core 鐨勮璁″師鍒欙細

```text
鏉冮檺鐐规槸瀛楃涓诧紝涓嶅啓姝?enum
璧勬簮绫诲瀷鏄瓧绗︿覆锛屼笉鍐欐 enum
瑙掕壊鏄暟鎹簱鏁版嵁锛屼笉鍐欐 enum
鎺堟潈鍏崇郴閫氳繃琛ㄧ淮鎶?璧勬簮缁ф壙閫氳繃 resource_edges 缁存姢
鐩存帴渚嬪閫氳繃 permission_assignments 缁存姢
涓氬姟鏈嶅姟鍙皟鐢ㄧ粺涓€妫€鏌ュ嚱鏁?```

杩欐牱鍙互閬垮厤鍚庣画姣忔鏂板涓€涓鍨嬨€佽禌鍒躲€佽繍钀ヨ兘鍔涢兘瑕佹敼鏉冮檺鏍稿績浠ｇ爜銆?
---

## 鍥涖€佹牳蹇冩蹇?
Permission Core 涓湁鍥涗釜鏈€鏍稿績姒傚康锛?
```text
Principal
Scope
Permission
Role
```

---

## 浜斻€丳rincipal 鏉冮檺涓讳綋

`Principal` 琛ㄧず鏉冮檺涓讳綋锛屼篃灏辨槸鈥滆皝鈥濄€?
缁撴瀯锛?
```text
principal_type
principal_id
```

褰撳墠涓昏浣跨敤锛?
```text
user:{id}
```

渚嬪锛?
```text
user:1
user:2
user:100
```

鏈潵鍙互鎵╁睍锛?
```text
team:{id}
group:{id}
service:{id}
```

绀轰緥锛?
```text
team:5
group:2
service:1
```

鎺ㄨ崘 Go 绫诲瀷锛?
```go
type Principal struct {
    Type string
    ID   int64
}
```

甯搁噺锛?
```go
const (
    PrincipalUser    = "user"
    PrincipalTeam    = "team"
    PrincipalGroup   = "group"
    PrincipalService = "service"
)
```

杈呭姪鍑芥暟锛?
```go
func UserPrincipal(userID int64) Principal {
    return Principal{
        Type: PrincipalUser,
        ID:   userID,
    }
}
```

褰撳墠 Permission Core 涓昏妫€鏌ョ敤鎴蜂富浣擄細

```text
user:{id}
```

浣嗕繚鐣?`team / group / service` 鐨勫師鍥犳槸鍚庣画浼氬嚭鐜帮細

```text
鍥㈤槦鍙傝禌
缁勭粐鏉冮檺
鏈嶅姟闂磋皟鐢?鏈哄櫒浜鸿处鍙?妯″潡鏈嶅姟璐﹀彿
```

渚嬪锛?
```text
team:9 -> contest_participant @ contest:5
group:1 -> problem_viewer @ problem:7
service:3 -> submission.rejudge @ system:0
```

---

## 鍏€丼cope 璧勬簮浣滅敤鍩?
`Scope` 琛ㄧず鏉冮檺浣滅敤鍩燂紝涔熷氨鏄€滃湪鍝噷鈥濄€?
缁撴瀯锛?
```text
scope_type
scope_id
```

绀轰緥锛?
```text
system:0
problem:7
contest:3
group:2
team:5
submission:100
module:0
balloon:12
print:20
post:30
clarification:40
```

鎺ㄨ崘 Go 绫诲瀷锛?
```go
type Scope struct {
    Type string
    ID   int64
}
```

甯搁噺锛?
```go
const (
    ScopeSystem = "system"
)
```

杈呭姪鍑芥暟锛?
```go
func SystemScope() Scope {
    return Scope{
        Type: ScopeSystem,
        ID:   0,
    }
}
```

鏍稿績绾﹀畾锛?
```text
system:0 琛ㄧず鍏ㄥ眬绯荤粺浣滅敤鍩?problem:0 琛ㄧず鎵€鏈夐鐩?contest:0 琛ㄧず鎵€鏈夋瘮璧?module:0 琛ㄧず鎵€鏈夋ā鍧?scope_id = 0 琛ㄧず璇ョ被鍨嬭祫婧愮殑鍏ㄥ眬鑼冨洿
```

渚嬪锛?
```text
problem.edit @ problem:7
```

琛ㄧず鍙兘缂栬緫绗?7 棰樸€?
```text
problem.edit @ problem:0
```

琛ㄧず鍙互缂栬緫鎵€鏈夐鐩€?
```text
contest.manage @ contest:5
```

琛ㄧず鍙互绠＄悊绗?5 鍦烘瘮璧涖€?
```text
contest.manage @ contest:0
```

琛ㄧず鍙互绠＄悊鎵€鏈夋瘮璧涖€?
```text
system.admin @ system:0
```

琛ㄧず绯荤粺鍏ㄥ眬绠＄悊鍛樿兘鍔涖€?
---

## 涓冦€丳ermission 鏉冮檺鐐?
`Permission` 琛ㄧず鍏蜂綋鎿嶄綔鑳藉姏锛屼篃灏辨槸鈥滆兘鍋氫粈涔堚€濄€?
鏉冮檺鐐逛娇鐢ㄥ瓧绗︿覆琛ㄧず銆?
鍛藉悕瑙勮寖锛?
```text
<domain>.<action>
<domain>.<subdomain>.<action>
```

绀轰緥锛?
```text
judge.submit
problem.create
problem.edit
problem.manage.data
problem.manage.asset
contest.create
contest.manage
contest.manage.participant
contest.manage.problem
contest.freeze
scoreboard.roll
balloon.manage
print.operate
module.install
```

鏉冮檺鐐逛笉搴旇鍐欐垚 Go enum銆?
鍘熷洜锛?
```text
鏈潵妯″潡鍙互娉ㄥ唽鏂版潈闄愮偣
鏉冮檺鐐归渶瑕佺敱鏁版嵁搴撳拰妯″潡 manifest 绠＄悊
涓嶅簲璇ユ瘡鏂板涓€涓ā鍧楀氨鏀规牳蹇冧唬鐮?```

鏉冮檺鐐瑰簲璇ュ啓鍏ワ細

```text
permissions
```

琛ㄤ腑銆?
鎺ㄨ崘鏉冮檺鐐瑰垎绫伙細

```text
system.*
module.*
launcher.*
problem.*
judge.*
submission.*
contest.*
scoreboard.*
balloon.*
print.*
forum.*
clarification.*
```

---

## 鍏€丷ole 瑙掕壊

`Role` 琛ㄧず鏉冮檺闆嗗悎妯℃澘銆?
瑙掕壊鏈韩涓嶅寘鍚綔鐢ㄥ煙銆?
渚嬪锛?
```text
contest_manager
```

琛ㄧず杩欎釜瑙掕壊鎷ユ湁涓€缁勬瘮璧涚鐞嗚兘鍔涳紝渚嬪锛?
```text
contest.manage
contest.manage.participant
contest.manage.problem
contest.freeze
scoreboard.view.admin
```

浣嗘槸鐢ㄦ埛鍦ㄥ摢涓瘮璧涗笂鎷ユ湁 `contest_manager`锛屼笉鐢?`roles` 琛ㄥ喅瀹氾紝鑰岀敱锛?
```text
role_bindings
```

鍐冲畾銆?
渚嬪锛?
```text
user:3 -> contest_manager @ contest:5
```

琛ㄧず鐢ㄦ埛 3 鏄瘮璧?5 鐨勬瘮璧涚鐞嗗憳銆?
瑙掕壊鍒嗕袱绫伙細

```text
绯荤粺绾ц鑹?璧勬簮绾ц鑹?```

绯荤粺绾ц鑹茬ず渚嬶細

```text
super_admin
admin
user
module_manager
```

璧勬簮绾ц鑹茬ず渚嬶細

```text
problem_owner
problem_setter
problem_viewer
problem_data_manager
contest_owner
contest_manager
contest_judge
contest_participant
balloon_volunteer
print_operator
forum_moderator
```

绯荤粺绾ц鑹查€氬父閫氳繃锛?
```text
user_roles
```

缁戝畾銆?
璧勬簮绾ц鑹查€氬父閫氳繃锛?
```text
role_bindings
```

缁戝畾銆?
---

## 涔濄€佹暟鎹簱缁撴瀯鎬昏

Permission Core 淇濈暀骞跺吋瀹瑰凡鏈夎〃锛?
```text
users
roles
user_roles
```

骞舵柊澧炴牳蹇冭〃锛?
```text
resource_types
permissions
role_permissions
role_bindings
permission_assignments
resource_edges
permission_audit_logs
```

鏁翠綋鍏崇郴锛?
```text
users
  鈫?user_roles
  鈫?roles
  鈫?role_permissions
  鈫?permissions

principals
  鈫?role_bindings
  鈫?roles
  鈫?role_permissions
  鈫?permissions

principals
  鈫?permission_assignments
  鈫?permissions

resource_edges
  鈫?scope inheritance
```

鍏朵腑锛?
```text
user_roles
```

琛ㄧず鐢ㄦ埛鐨勭郴缁熺骇鍏ㄥ眬瑙掕壊銆?
```text
role_bindings
```

琛ㄧず鏌愪釜鏉冮檺涓讳綋鍦ㄦ煇涓祫婧愪綔鐢ㄥ煙涓婃嫢鏈夋煇涓鑹层€?
```text
permission_assignments
```

琛ㄧず鐩存帴 allow 鎴?deny 鏌愪釜鏉冮檺銆?
```text
resource_edges
```

琛ㄧず璧勬簮涔嬮棿鐨勭户鎵垮叧绯汇€?
---

## 鍗併€乺esource_types 琛?
`resource_types` 鏄祫婧愮被鍨嬫敞鍐岃〃銆?
鐢ㄩ€旓細

```text
澹版槑绯荤粺涓湁鍝簺璧勬簮绫诲瀷
鏀寔妯″潡娉ㄥ唽鑷繁鐨勮祫婧愮被鍨?閬垮厤鍦ㄤ唬鐮佷腑鍐欐璧勬簮绫诲瀷 enum
```

鍏稿瀷鏁版嵁锛?
```text
system
module
problem
contest
group
team
submission
post
clarification
balloon
print
```

鏈潵鍙兘鏂板锛?
```text
training
homework
course
virtual_contest
dataset
checker
runner
language_pack
```

鎺ㄨ崘瀛楁锛?
```text
code
module_code
name
description
created_at
```

瀛楁璇存槑锛?
| 瀛楁            | 鍚箟                     |
| ------------- | ---------------------- |
| `code`        | 璧勬簮绫诲瀷浠ｇ爜锛屼緥濡?`problem`    |
| `module_code` | 鎵€灞炴ā鍧楋紝渚嬪 `problem-core` |
| `name`        | 灞曠ず鍚嶇О                   |
| `description` | 璇存槑                     |
| `created_at`  | 鍒涘缓鏃堕棿                   |

绀轰緥锛?
```sql
INSERT INTO resource_types(code, module_code, name, description)
VALUES
    ('problem', 'problem-core', 'Problem', '棰樼洰璧勬簮'),
    ('contest', 'contest-core', 'Contest', '姣旇禌璧勬簮'),
    ('module', 'module-registry', 'Module', '妯″潡璧勬簮')
ON CONFLICT(code) DO NOTHING;
```

鍚庣画妯″潡瀹夎鏃讹紝Launcher 鍙互鏍规嵁 `ojos.module.yaml` 鑷姩娉ㄥ唽 resource_types銆?
---

## 鍗佷竴銆乸ermissions 琛?
`permissions` 鏄潈闄愮偣娉ㄥ唽琛ㄣ€?
鐢ㄩ€旓細

```text
澹版槑绯荤粺涓湁鍝簺鏉冮檺鐐?鏀寔妯″潡娉ㄥ唽鑷繁鐨勬潈闄愮偣
閬垮厤鍦ㄤ唬鐮佷腑鍐欐鏉冮檺鐐?enum
```

鎺ㄨ崘瀛楁锛?
```text
code
module_code
name
description
created_at
```

瀛楁璇存槑锛?
| 瀛楁            | 鍚箟                      |
| ------------- | ----------------------- |
| `code`        | 鏉冮檺鐐逛唬鐮侊紝渚嬪 `judge.submit` |
| `module_code` | 鎵€灞炴ā鍧楋紝渚嬪 `judge-core`    |
| `name`        | 灞曠ず鍚嶇О                    |
| `description` | 璇存槑                      |
| `created_at`  | 鍒涘缓鏃堕棿                    |

鍏稿瀷鏉冮檺鐐癸細

```text
system.admin

module.install
module.enable
module.disable
module.configure

launcher.view
launcher.install
launcher.uninstall
launcher.enable
launcher.disable

problem.create
problem.view
problem.view.private
problem.edit
problem.delete
problem.manage.data
problem.manage.asset

judge.submit

submission.view.own
submission.view.all
submission.rejudge
submission.delete

contest.create
contest.view
contest.manage
contest.manage.participant
contest.manage.problem
contest.freeze
contest.roll
contest.publish

scoreboard.view
scoreboard.view.admin
scoreboard.freeze
scoreboard.roll
scoreboard.export

balloon.manage
balloon.deliver

print.request
print.manage
print.operate

forum.post
forum.moderate

clarification.ask
clarification.answer
clarification.publish
```

绀轰緥锛?
```sql
INSERT INTO permissions(code, module_code, name, description)
VALUES
    ('judge.submit', 'judge-core', 'Submit Code', '鎻愪氦浠ｇ爜'),
    ('problem.create', 'problem-core', 'Create Problem', '鍒涘缓棰樼洰'),
    ('problem.edit', 'problem-core', 'Edit Problem', '缂栬緫棰樼洰')
ON CONFLICT(code) DO NOTHING;
```

---

## 鍗佷簩銆乺oles 琛?
`roles` 鏄鑹茶〃銆?
绯荤粺淇濈暀宸叉湁 `roles`锛屽苟鎵╁睍閫氱敤瀛楁锛?
```text
id
name
module_code
description
is_system
created_at
```

瀛楁璇存槑锛?
| 瀛楁            | 鍚箟       |
| ------------- | -------- |
| `id`          | 瑙掕壊 ID    |
| `name`        | 瑙掕壊鍚?     |
| `module_code` | 鎵€灞炴ā鍧?    |
| `description` | 鎻忚堪       |
| `is_system`   | 鏄惁绯荤粺鍐呯疆瑙掕壊 |
| `created_at`  | 鍒涘缓鏃堕棿     |

褰撳墠鍐呯疆瑙掕壊寤鸿鍖呮嫭锛?
```text
super_admin
admin
user

module_manager

problem_owner
problem_setter
problem_viewer
problem_data_manager

contest_owner
contest_manager
contest_judge
contest_participant

balloon_volunteer
print_operator
forum_moderator
```

瑙掕壊鍛藉悕瑙勮寖锛?
```text
snake_case
```

绀轰緥锛?
```text
problem_owner
contest_manager
balloon_volunteer
```

涓嶆帹鑽愶細

```text
ProblemOwner
contest-manager
CONTEST_MANAGER
```

---

## 鍗佷笁銆乺ole_permissions 琛?
`role_permissions` 琛ㄧず鏌愪釜瑙掕壊鎷ユ湁鍝簺鏉冮檺鐐广€?
瀛楁锛?
```text
role_id
permission_code
created_at
```

鎺ㄨ崘涓婚敭鎴栧敮涓€绾︽潫锛?
```text
(role_id, permission_code)
```

娉ㄦ剰锛?
```text
role_permissions 涓嶅甫 scope
```

鍘熷洜鏄細

```text
瑙掕壊鍙畾涔夎兘鍔涙ā鏉?浣滅敤鍩熺敱 user_roles 鎴?role_bindings 鍐冲畾
```

渚嬪锛?
```text
contest_manager 鎷ユ湁 contest.manage
contest_manager 鎷ユ湁 contest.freeze
contest_manager 鎷ユ湁 scoreboard.view.admin
```

浣嗘槸鐢ㄦ埛鍦ㄥ摢涓瘮璧涗笂鏄?contest_manager锛岀敱锛?
```text
role_bindings
```

鍐冲畾銆?
绀轰緥锛?
```sql
INSERT INTO role_permissions(role_id, permission_code)
SELECT r.id, 'judge.submit'
FROM roles r
WHERE r.name = 'user'
ON CONFLICT DO NOTHING;
```

琛ㄧず锛?
```text
user 瑙掕壊鎷ユ湁 judge.submit
```

杩欏苟涓嶄唬琛ㄦ煇涓敤鎴蜂竴瀹氭嫢鏈夎瑙掕壊锛岃繕闇€瑕?`user_roles` 鎴?`role_bindings` 缁戝畾銆?
---

## 鍗佸洓銆乽ser_roles 琛?
`user_roles` 鏄凡鏈夎〃锛岀敤浜庣淮鎶ょ敤鎴蜂笌绯荤粺绾ц鑹茬殑鍏崇郴銆?
褰撳墠瀹氫箟涓猴細

```text
鐢ㄦ埛鐨勭郴缁熺骇鍏ㄥ眬瑙掕壊缁戝畾
```

瀛楁锛?
```text
user_id
role_id
```

绀轰緥锛?
```text
permtest -> user
admin -> user
admin -> super_admin
```

`user_roles` 涓殑瑙掕壊鏄叏灞€鐨勩€?
渚嬪锛?
```text
user:2 -> user
```

琛ㄧず鐢ㄦ埛 2 鎷ユ湁 `user` 杩欎釜绯荤粺绾ц鑹层€?
濡傛灉 `user` 瑙掕壊閫氳繃 `role_permissions` 鎷ユ湁锛?
```text
judge.submit
```

鍒欑敤鎴?2 榛樿鎷ユ湁锛?
```text
judge.submit @ system:0
```

褰撳墠娉ㄥ唽鐢ㄦ埛榛樿缁戝畾锛?
```text
user
```

杩欑敱 Auth 妯″潡瀹屾垚銆?
---

## 鍗佷簲銆乺ole_bindings 琛?
`role_bindings` 鏄祫婧愮骇瑙掕壊缁戝畾琛ㄣ€?
鐢ㄩ€旓細

```text
澹版槑鏌愪釜鏉冮檺涓讳綋鍦ㄦ煇涓祫婧愯寖鍥村唴鎷ユ湁鏌愪釜瑙掕壊
```

瀛楁锛?
```text
id

principal_type
principal_id

role_id

scope_type
scope_id

granted_by_type
granted_by_id

expires_at
created_at
```

鎺ㄨ崘鍞竴绾︽潫锛?
```text
(principal_type, principal_id, role_id, scope_type, scope_id)
```

绀轰緥锛?
```text
user:2 -> problem_setter @ problem:7
user:3 -> contest_manager @ contest:5
user:4 -> balloon_volunteer @ contest:5
team:9 -> contest_participant @ contest:5
```

瑙ｉ噴锛?
```text
user:2 鏄?problem:7 鐨勫嚭棰樹汉
user:3 鏄?contest:5 鐨勬瘮璧涚鐞嗗憳
user:4 鏄?contest:5 鐨勬皵鐞冨織鎰胯€?team:9 鏄?contest:5 鐨勫弬璧涢槦浼?```

SQL 绀轰緥锛?
```sql
INSERT INTO role_bindings(
    principal_type,
    principal_id,
    role_id,
    scope_type,
    scope_id,
    granted_by_type,
    granted_by_id
)
SELECT
    'user',
    2,
    r.id,
    'problem',
    7,
    'user',
    1
FROM roles r
WHERE r.name = 'problem_setter'
ON CONFLICT DO NOTHING;
```

---

## 鍗佸叚銆乸ermission_assignments 琛?
`permission_assignments` 鏄洿鎺ユ巿鏉?/ 鐩存帴鎷掔粷琛ㄣ€?
鐢ㄩ€旓細

```text
澶勭悊渚嬪鏉冮檺
涓存椂鎺堟潈
涓存椂绂佹
灏佺鐢ㄦ埛
瑕嗙洊鏅€氳鑹叉潈闄?鐗规畩鎿嶄綔鎺堟潈
```

瀛楁锛?
```text
id

principal_type
principal_id

permission_code

scope_type
scope_id

effect

granted_by_type
granted_by_id

reason
expires_at
created_at
```

`effect` 鍙兘鏄細

```text
allow
deny
```

鎺ㄨ崘鍞竴绾︽潫锛?
```text
(principal_type, principal_id, permission_code, scope_type, scope_id)
```

绀轰緥锛?
```text
allow user:5 problem.edit @ problem:9
deny  user:6 contest.view @ contest:3
deny  user:7 judge.submit @ system:0
allow user:8 scoreboard.roll @ contest:5
```

褰撳墠鐪熷疄楠岃瘉涓娇鐢ㄨ繃锛?
```text
deny user:permtest judge.submit @ system:0
```

鍐欏叆鍚庯紝permtest 鍐嶆彁浜や唬鐮佷細琚嫤鎴€?
鍒犻櫎 deny 鍚庯紝permtest 閫氳繃 `user` 瑙掕壊閲嶆柊鑾峰緱 `judge.submit`锛屾彁浜ゆ仮澶嶆甯搞€?
---

### 16.1 deny 鐨勪紭鍏堢骇

褰撳墠瑙勫垯锛?
```text
deny 浼樺厛浜庢櫘閫?allow 鍜岃鑹叉潈闄?```

涔熷氨鏄锛?
```text
鐢ㄦ埛閫氳繃 user 瑙掕壊鎷ユ湁 judge.submit
浣?permission_assignments 涓瓨鍦?judge.submit deny
鍒欐渶缁堟嫆缁?```

浣嗘槸锛?
```text
super_admin 楂樹簬 deny
```

濡傛灉鐢ㄦ埛鎷ユ湁 `super_admin`锛屽垯鐩存帴鍏佽锛屼笉妫€鏌?deny銆?
鍘熷洜鏄細

```text
super_admin 鏄郴缁熸渶楂樻潈闄?濡傛灉瑕侀檺鍒?super_admin锛屽簲璇ョЩ闄?super_admin 瑙掕壊
涓嶅簲璇ョ敤 deny 鍘昏鐩?super_admin
```

---

### 16.2 expires_at

`permission_assignments` 鏀寔锛?
```text
expires_at
```

鐢ㄤ簬涓存椂鎺堟潈鎴栦复鏃舵嫆缁濄€?
渚嬪锛?
```text
涓存椂绂佹鐢ㄦ埛鎻愪氦 24 灏忔椂
涓存椂鍏佽鐢ㄦ埛绠＄悊鏌愬満姣旇禌
涓存椂鎺堜簣楠岄鏉冮檺
```

鏉冮檺鍒ゆ柇鏃跺簲蹇界暐宸茬粡杩囨湡鐨勮褰曪細

```sql
expires_at IS NULL OR expires_at > NOW()
```

---

## 鍗佷竷銆乺esource_edges 琛?
`resource_edges` 鐢ㄤ簬琛ㄨ揪璧勬簮缁ф壙鍏崇郴銆?
鐢ㄩ€旓細

```text
鏀寔璧勬簮绾ф潈闄愮户鎵?```

瀛楁锛?
```text
id

parent_type
parent_id

child_type
child_id

relation
created_at
```

鎺ㄨ崘鍞竴绾︽潫锛?
```text
(parent_type, parent_id, child_type, child_id, relation)
```

绀轰緥锛?
```text
group:1   -> contest:3
contest:3 -> problem:7
contest:3 -> submission:100
contest:3 -> balloon:12
contest:3 -> print:20
```

鍚箟锛?
```text
group:1 鍖呭惈 contest:3
contest:3 鍖呭惈 problem:7
contest:3 鍖呭惈 submission:100
contest:3 鍖呭惈 balloon:12
contest:3 鍖呭惈 print:20
```

杩欐牱鍙互鏀寔锛?
```text
鐢ㄦ埛鏄?contest:3 鐨?contest_manager
鍥犳鍙互绠＄悊 contest:3 涓嬬殑 submission / balloon / print
```

璧勬簮缁ф壙鏌ヨ鏃讹紝搴斾粠褰撳墠 scope 鍚戠埗绾ч€掑綊銆?
渚嬪妫€鏌ワ細

```text
submission.view.all @ submission:100
```

濡傛灉瀛樺湪锛?
```text
contest:3 -> submission:100
```

鍒欏€欓€?scope 鍖呮嫭锛?
```text
submission:100
submission:0
contest:3
contest:0
system:0
```

濡傛灉鐢ㄦ埛鎷ユ湁锛?
```text
contest_manager @ contest:3
```

涓?`contest_manager` 鎷ユ湁锛?
```text
submission.view.all
```

鍒欏厑璁搞€?
---

## 鍗佸叓銆乸ermission_audit_logs 琛?
`permission_audit_logs` 鏄潈闄愬璁℃棩蹇楄〃銆?
鐢ㄩ€旓細

```text
璁板綍鏉冮檺鍙樻洿鍘嗗彶
璁板綍瑙掕壊缁戝畾鍘嗗彶
璁板綍鐩存帴鎺堟潈鎴栨嫆缁濆巻鍙?鏀寔鍚庡彴杩借釜
鏀寔闂鎺掓煡
鏀寔瀹夊叏瀹¤
```

瀛楁锛?
```text
id

actor_type
actor_id

action

target_type
target_id

permission_code
role_id
role_name

scope_type
scope_id

effect
metadata

created_at
```

鍏稿瀷 action锛?
```text
role.bind
role.revoke
permission.assign
permission.revoke
resource.edge.add
resource.edge.remove
permission.register
resource_type.register
```

绀轰緥锛?
```text
user:1 缁?user:2 缁戝畾 problem_setter @ problem:7
user:1 缁?user:3 deny judge.submit @ system:0
user:1 娣诲姞 contest:5 -> problem:7 璧勬簮鍏崇郴
```

褰撳墠宸叉湁琛ㄥ拰鍩虹鍐欏叆鑳藉姏锛屼絾缂哄皯锛?
```text
瀹¤鏃ュ織鏌ヨ API
鏉冮檺鍚庡彴 UI
绛涢€夊拰鍒嗛〉
```

鍚庣画搴旂敱 `permission-api` 鎴栧悗鍙扮鐞嗘ā鍧楀疄鐜般€?
---

## 鍗佷節銆佹潈闄愬垽鏂鍒?
缁熶竴妫€鏌ュ嚱鏁帮細

```text
HasPermission(principal, permission, scope)
```

鐢ㄦ埛鍦烘櫙涓娇鐢細

```text
HasUserPermission(user_id, permission, scope)
```

涓氬姟鏈嶅姟甯哥敤锛?
```text
RequireUserPermission(user_id, permission, scope)
```

濡傛灉鏃犳潈闄愶紝鍒欒繑鍥烇細

```text
ErrForbidden
```

褰撳墠鍒ゆ柇椤哄簭濡備笅銆?
---

### 19.1 super_admin 鏈€楂樹紭鍏堢骇

濡傛灉鐢ㄦ埛鎷ユ湁锛?
```text
super_admin
```

鍒欑洿鎺ュ厑璁告墍鏈夋潈闄愩€?
杩欎釜鍒ゆ柇浼樺厛浜?deny銆?
濡傛灉瑕佹挙閿€瓒呯骇鏉冮檺锛屽簲绉婚櫎鐢ㄦ埛鐨?`super_admin` 瑙掕壊锛岃€屼笉鏄啓 deny銆?
渚嬪锛?
```sql
DELETE FROM user_roles
WHERE user_id = 1
  AND role_id = (SELECT id FROM roles WHERE name = 'super_admin');
```

---

### 19.2 鏀堕泦鍊欓€変綔鐢ㄥ煙

渚嬪妫€鏌ワ細

```text
problem.edit @ problem:7
```

Permission Core 浼氭敹闆嗗€欓€変綔鐢ㄥ煙锛?
```text
problem:7
problem:0
parent scopes...
system:0
```

濡傛灉 `resource_edges` 涓瓨鍦細

```text
contest:3 -> problem:7
group:1 -> contest:3
```

鍒欏€欓€変綔鐢ㄥ煙鍙兘鍖呮嫭锛?
```text
problem:7
problem:0
contest:3
contest:0
group:1
group:0
system:0
```

鍏朵腑锛?
```text
type:0
```

琛ㄧず鏌愮被璧勬簮鐨勫叏灞€浣滅敤鍩熴€?
渚嬪锛?
```text
problem:0
contest:0
group:0
```

---

### 19.3 妫€鏌ョ洿鎺?deny

妫€鏌ワ細

```text
permission_assignments.effect = deny
```

骞朵笖锛?
```text
principal 鍖归厤
permission_code 鍖归厤
scope 鍦ㄥ€欓€変綔鐢ㄥ煙鍐?expires_at 鏈繃鏈?```

濡傛灉鍛戒腑锛岀洿鎺ユ嫆缁濄€?
---

### 19.4 妫€鏌ョ洿鎺?allow

妫€鏌ワ細

```text
permission_assignments.effect = allow
```

鏉′欢鍚屼笂銆?
濡傛灉鍛戒腑锛屽厑璁搞€?
---

### 19.5 妫€鏌ュ叏灞€ user_roles

妫€鏌ョ敤鎴锋槸鍚﹂€氳繃 `user_roles` 鎷ユ湁鏌愪釜瑙掕壊锛屽苟涓旇瑙掕壊閫氳繃 `role_permissions` 鎷ユ湁鐩爣鏉冮檺銆?
渚嬪锛?
```text
user:2 -> user
user -> judge.submit
```

鍒欙細

```text
user:2 has judge.submit @ system:0
```

褰撳墠鏅€氱敤鎴锋彁浜や唬鐮佸氨鏄€氳繃杩欎釜鏈哄埗瀹炵幇銆?
---

### 19.6 妫€鏌ヨ祫婧愮骇 role_bindings

妫€鏌ョ敤鎴锋槸鍚﹀湪鍊欓€変綔鐢ㄥ煙涓婃嫢鏈夋煇涓鑹诧細

```text
role_bindings.scope_type / scope_id in candidate scopes
```

鍐嶆鏌ヨ瑙掕壊鏄惁鎷ユ湁鐩爣鏉冮檺锛?
```text
role_permissions.permission_code = target permission
```

鍛戒腑鍒欏厑璁搞€?
渚嬪锛?
```text
user:2 -> problem_setter @ problem:7
problem_setter -> problem.edit
```

鍒欙細

```text
user:2 鍙互 problem.edit @ problem:7
```

---

### 19.7 榛樿鎷掔粷

濡傛灉浠ヤ笂瑙勫垯閮芥病鏈夊懡涓紝鍒欐嫆缁濄€?
榛樿鎷掔粷鏄潈闄愮郴缁熺殑鍩烘湰鍘熷垯銆?
涔熷氨鏄锛屼笉鑳藉洜涓烘病鏈夐厤缃?deny 灏卞厑璁搞€?
鍙湁鏄庣‘ allow銆佽鑹叉巿鏉冩垨 super_admin 鎵嶅厑璁搞€?
---

## 浜屽崄銆乻hared/security/permission API

璺緞锛?
```text
services/shared/security/permission
```

褰撳墠 Permission Core 浠?Go 鍖呭舰寮忔彁渚涚粰鍚勪笟鍔℃湇鍔′娇鐢ㄣ€?
---

### 20.1 Principal

鎺ㄨ崘缁撴瀯锛?
```go
type Principal struct {
    Type string
    ID   int64
}
```

杈呭姪鍑芥暟锛?
```go
func UserPrincipal(userID int64) Principal
```

绀轰緥锛?
```go
principal := permission.UserPrincipal(userID)
```

---

### 20.2 Scope

鎺ㄨ崘缁撴瀯锛?
```go
type Scope struct {
    Type string
    ID   int64
}
```

杈呭姪鍑芥暟锛?
```go
func SystemScope() Scope
```

绀轰緥锛?
```go
scope := permission.SystemScope()
```

棰樼洰浣滅敤鍩燂細

```go
scope := permission.Scope{
    Type: "problem",
    ID:   problemID,
}
```

姣旇禌浣滅敤鍩燂細

```go
scope := permission.Scope{
    Type: "contest",
    ID:   contestID,
}
```

---

### 20.3 HasUserPermission

鍑芥暟锛?
```go
func HasUserPermission(
    ctx context.Context,
    db *pgxpool.Pool,
    userID int64,
    permissionCode string,
    scope Scope,
) (bool, error)
```

鐢ㄩ€旓細

```text
妫€鏌ユ煇涓敤鎴锋槸鍚︽嫢鏈夋寚瀹氭潈闄?```

绀轰緥锛?
```go
ok, err := permission.HasUserPermission(
    ctx,
    db,
    userID,
    "problem.edit",
    permission.Scope{
        Type: "problem",
        ID:   problemID,
    },
)
if err != nil {
    return err
}
if !ok {
    return permission.ErrForbidden
}
```

---

### 20.4 RequireUserPermission

鍑芥暟锛?
```go
func RequireUserPermission(
    ctx context.Context,
    db *pgxpool.Pool,
    userID int64,
    permissionCode string,
    scope Scope,
) error
```

鐢ㄩ€旓細

```text
鏉冮檺涓嶈冻鏃剁洿鎺ヨ繑鍥?ErrForbidden
```

绀轰緥锛?
```go
if err := permission.RequireUserPermission(
    ctx,
    db,
    userID,
    "judge.submit",
    permission.SystemScope(),
); err != nil {
    return nil, err
}
```

涓氬姟鏈嶅姟涓帹鑽愪紭鍏堜娇鐢?`RequireUserPermission`銆?
---

### 20.5 HasPermission

鍑芥暟锛?
```go
func HasPermission(
    ctx context.Context,
    db *pgxpool.Pool,
    principal Principal,
    permissionCode string,
    scope Scope,
) (bool, error)
```

鐢ㄩ€旓細

```text
鏀寔闈炵敤鎴蜂富浣擄紝渚嬪 team / group / service
```

褰撳墠涓昏浣跨敤鐢ㄦ埛涓讳綋銆?
鏈潵 team-based contest 鍙互鐢細

```go
permission.HasPermission(
    ctx,
    db,
    permission.Principal{
        Type: "team",
        ID:   teamID,
    },
    "contest.participate",
    permission.Scope{
        Type: "contest",
        ID:   contestID,
    },
)
```

---

### 20.6 BindRole

鍑芥暟锛?
```go
func BindRole(
    ctx context.Context,
    db *pgxpool.Pool,
    actor Principal,
    target Principal,
    roleName string,
    scope Scope,
    expiresAt *time.Time,
) error
```

鐢ㄩ€旓細

```text
缁欐煇涓富浣撳湪鏌愪釜璧勬簮浣滅敤鍩熶笂缁戝畾瑙掕壊
```

绀轰緥锛?
```go
err := permission.BindRole(
    ctx,
    db,
    permission.UserPrincipal(adminID),
    permission.UserPrincipal(userID),
    "problem_setter",
    permission.Scope{
        Type: "problem",
        ID:   problemID,
    },
    nil,
)
```

琛ㄧず锛?
```text
user:{userID} 鏄?problem:{problemID} 鐨?problem_setter
```

---

### 20.7 AssignPermission

鍑芥暟锛?
```go
func AssignPermission(
    ctx context.Context,
    db *pgxpool.Pool,
    actor Principal,
    target Principal,
    permissionCode string,
    scope Scope,
    effect string,
    reason string,
    expiresAt *time.Time,
) error
```

鐢ㄩ€旓細

```text
鐩存帴鍏佽鎴栨嫆缁濇煇涓富浣撳湪鏌愪釜浣滅敤鍩熶笂鐨勬煇涓潈闄?```

绀轰緥锛?
```go
err := permission.AssignPermission(
    ctx,
    db,
    permission.UserPrincipal(adminID),
    permission.UserPrincipal(userID),
    "judge.submit",
    permission.SystemScope(),
    permission.EffectDeny,
    "temporary banned",
    nil,
)
```

琛ㄧず锛?
```text
鐩存帴绂佹 user:{userID} 鍦?system:0 涓?judge.submit
```

---

### 20.8 AddResourceEdge

鍑芥暟锛?
```go
func AddResourceEdge(
    ctx context.Context,
    db *pgxpool.Pool,
    parent Scope,
    child Scope,
    relation string,
) error
```

鐢ㄩ€旓細

```text
寤虹珛璧勬簮缁ф壙鍏崇郴
```

绀轰緥锛?
```go
err := permission.AddResourceEdge(
    ctx,
    db,
    permission.Scope{
        Type: "contest",
        ID:   contestID,
    },
    permission.Scope{
        Type: "problem",
        ID:   problemID,
    },
    "contains",
)
```

琛ㄧず锛?
```text
contest:{contestID} contains problem:{problemID}
```

---

### 20.9 RegisterResourceType

鍑芥暟锛?
```go
func RegisterResourceType(
    ctx context.Context,
    db *pgxpool.Pool,
    code string,
    moduleCode string,
    name string,
    description string,
) error
```

鐢ㄩ€旓細

```text
妯″潡娉ㄥ唽鑷繁鐨勮祫婧愮被鍨?```

绀轰緥锛?
```go
permission.RegisterResourceType(
    ctx,
    db,
    "training",
    "training-core",
    "Training",
    "璁粌璧勬簮",
)
```

---

### 20.10 RegisterPermission

鍑芥暟锛?
```go
func RegisterPermission(
    ctx context.Context,
    db *pgxpool.Pool,
    code string,
    moduleCode string,
    name string,
    description string,
) error
```

鐢ㄩ€旓細

```text
妯″潡娉ㄥ唽鑷繁鐨勬潈闄愮偣
```

绀轰緥锛?
```go
permission.RegisterPermission(
    ctx,
    db,
    "training.manage",
    "training-core",
    "Manage Training",
    "绠＄悊璁粌",
)
```

---

### 20.11 GrantRolePermission

鍑芥暟锛?
```go
func GrantRolePermission(
    ctx context.Context,
    db *pgxpool.Pool,
    roleName string,
    permissionCode string,
) error
```

鐢ㄩ€旓細

```text
缁欒鑹叉巿浜堟潈闄愮偣
```

绀轰緥锛?
```go
permission.GrantRolePermission(
    ctx,
    db,
    "training_manager",
    "training.manage",
)
```

---

## 浜屽崄涓€銆佷笟鍔℃湇鍔℃帴鍏ユ柟寮?
### 21.1 Gateway

Gateway 涓嶅仛璧勬簮绾ф潈闄愬垽鏂€?
Gateway 鍙礋璐ｏ細

```text
JWT 楠岃瘉
鐢ㄦ埛涓婁笅鏂囬€忎紶
```

Gateway 涓嶅簲璇ュ啓锛?
```text
check problem.edit
check judge.submit
check contest.manage
```

杩欎簺鐢变笟鍔℃湇鍔¤皟鐢?Permission Core 瀹屾垚銆?
---

### 21.2 Auth

Auth 涓嶅仛璧勬簮绾ф潈闄愬垽鏂€?
Auth 璐熻矗锛?
```text
娉ㄥ唽
鐧诲綍
JWT
鍩虹瑙掕壊
```

Auth 娉ㄥ唽鐢ㄦ埛鏃剁粦瀹氾細

```text
user
```

璧勬簮绾ф潈闄愮敱 Permission Core 鍒ゆ柇銆?
---

### 21.3 judge-api

褰撳墠宸叉帴鍏ワ細

```text
POST /judge/submissions
    -> judge.submit @ system:0
```

閫昏緫锛?
```text
浠?authctx 璇诲彇 user_id
璋冪敤 RequireUserPermission
鏉冮檺閫氳繃鍚庡垱寤?submission
鍐欏叆 Redis Stream
```

绀轰緥锛?
```go
user, ok := authctx.FromContext(l.ctx)
if !ok || user == nil || user.UserID <= 0 {
    return nil, errors.New("unauthorized")
}

if err := permission.RequireUserPermission(
    l.ctx,
    l.svcCtx.DB,
    user.UserID,
    "judge.submit",
    permission.SystemScope(),
); err != nil {
    return nil, err
}
```

---

### 21.4 problem-api

鍚庣画搴旀帴鍏ワ細

```text
POST /problem/problems
    -> problem.create @ system:0

GET /problem/problems/:id
    -> problem.view @ problem:{id}

PUT /problem/problems/:id
    -> problem.edit @ problem:{id}

POST /problem/problems/:id/testcases
    -> problem.manage.data @ problem:{id}

POST /problem/problems/:id/assets
    -> problem.manage.asset @ problem:{id}
```

棰樼洰鍒涘缓鎴愬姛鍚庡簲鑷姩缁戝畾锛?
```text
creator -> problem_owner @ problem:{id}
```

鍗筹細

```go
permission.BindRole(
    ctx,
    db,
    permission.UserPrincipal(creatorID),
    permission.UserPrincipal(creatorID),
    "problem_owner",
    permission.Scope{Type: "problem", ID: problemID},
    nil,
)
```

---

### 21.5 contest-api

鍚庣画搴旀帴鍏ワ細

```text
POST /contest/contests
    -> contest.create @ system:0

GET /contest/contests/:id
    -> contest.view @ contest:{id}

PUT /contest/contests/:id
    -> contest.manage @ contest:{id}

POST /contest/contests/:id/problems
    -> contest.manage.problem @ contest:{id}

POST /contest/contests/:id/participants
    -> contest.manage.participant @ contest:{id}

POST /contest/contests/:id/freeze
    -> contest.freeze @ contest:{id}

POST /contest/contests/:id/roll
    -> contest.roll @ contest:{id}
```

姣旇禌鍒涘缓鎴愬姛鍚庡簲鑷姩缁戝畾锛?
```text
creator -> contest_owner @ contest:{id}
```

姣旇禌娣诲姞棰樼洰鍚庡簲鍐欏叆锛?
```text
contest:{id} -> problem:{id}
```

鍒?`resource_edges`銆?
---

### 21.6 scoreboard-api

鍚庣画搴旀帴鍏ワ細

```text
GET /scoreboard/contests/:id
    -> scoreboard.view @ contest:{id}

GET /scoreboard/contests/:id/admin
    -> scoreboard.view.admin @ contest:{id}

POST /scoreboard/contests/:id/freeze
    -> scoreboard.freeze @ contest:{id}

POST /scoreboard/contests/:id/roll
    -> scoreboard.roll @ contest:{id}

GET /scoreboard/contests/:id/export
    -> scoreboard.export @ contest:{id}
```

---

### 21.7 balloon-service

鍚庣画搴旀帴鍏ワ細

```text
GET /balloon/contests/:id/tasks
    -> balloon.manage @ contest:{id}

POST /balloon/tasks/:id/deliver
    -> balloon.deliver @ contest:{id}
```

---

### 21.8 print-service

鍚庣画搴旀帴鍏ワ細

```text
POST /print/contests/:id/requests
    -> print.request @ contest:{id}

GET /print/contests/:id/requests
    -> print.manage @ contest:{id}

POST /print/requests/:id/operate
    -> print.operate @ contest:{id}
```

---

### 21.9 launcher

鍚庣画搴旀帴鍏ワ細

```text
GET /launcher/modules
    -> launcher.view @ system:0

POST /launcher/install
    -> launcher.install @ system:0

POST /launcher/uninstall
    -> launcher.uninstall @ system:0

POST /launcher/enable
    -> launcher.enable @ system:0

POST /launcher/disable
    -> launcher.disable @ system:0
```

---

## 浜屽崄浜屻€佸綋鍓嶇湡瀹為獙鏀剁粨鏋?
褰撳墠宸茬粡鐪熷疄楠岃瘉 Permission Core 鐨勫熀纭€閾捐矾銆?
娴嬭瘯鐢ㄦ埛锛?
```text
permtest
```

瑙掕壊锛?
```text
user
```

楠岃瘉鍐呭锛?
```text
1. permtest 鍙湁 user 瑙掕壊
2. 娌℃湁 deny 鏃讹紝permtest 鍙互鎻愪氦浠ｇ爜
3. submission 姝ｇ‘鍐欏叆 user_id
4. judge-worker 姝ｅ父鍒ら
5. submission 鏈€缁?ACCEPTED
6. 鍐欏叆 judge.submit @ system:0 deny
7. permtest 鍐嶆彁浜よ forbidden 鎷︽埅
8. 鍒犻櫎 deny
9. permtest 鍐嶆鎻愪氦鎭㈠姝ｅ父
10. submission 鍐嶆 ACCEPTED
```

杩欒鏄庯細

```text
user 瑙掕壊閫氳繃 role_permissions 鑾峰緱 judge.submit
judge-api 瀹為檯璋冪敤浜?RequireUserPermission
permission_assignments.deny 鍙互瑕嗙洊鏅€氳鑹叉潈闄?鍒犻櫎 deny 鍚庤鑹叉潈闄愭仮澶?```

---

## 浜屽崄涓夈€侀獙鏀?SQL

### 23.1 鏌ョ湅鐢ㄦ埛瑙掕壊

```sql
SELECT u.id, u.username, r.name
FROM users u
JOIN user_roles ur ON ur.user_id = u.id
JOIN roles r ON r.id = ur.role_id
WHERE u.username = 'permtest'
ORDER BY r.name;
```

棰勬湡锛?
```text
permtest | user
```

---

### 23.2 鍐欏叆 deny

```sql
INSERT INTO permission_assignments(
    principal_type,
    principal_id,
    permission_code,
    scope_type,
    scope_id,
    effect,
    reason
)
SELECT
    'user',
    u.id,
    'judge.submit',
    'system',
    0,
    'deny',
    'test deny judge.submit'
FROM users u
WHERE u.username = 'permtest'
ON CONFLICT(principal_type, principal_id, permission_code, scope_type, scope_id)
DO UPDATE SET
    effect = EXCLUDED.effect,
    reason = EXCLUDED.reason;
```

鍐欏叆鍚庯紝permtest 鎻愪氦搴旇鎷掔粷銆?
---

### 23.3 鏌ョ湅 deny

```sql
SELECT
    pa.principal_type,
    u.username,
    pa.permission_code,
    pa.scope_type,
    pa.scope_id,
    pa.effect,
    pa.reason
FROM permission_assignments pa
JOIN users u ON u.id = pa.principal_id
WHERE pa.principal_type = 'user'
  AND u.username = 'permtest';
```

棰勬湡锛?
```text
user | permtest | judge.submit | system | 0 | deny
```

---

### 23.4 鍒犻櫎 deny

```sql
DELETE FROM permission_assignments
WHERE principal_type = 'user'
  AND principal_id = (SELECT id FROM users WHERE username = 'permtest')
  AND permission_code = 'judge.submit'
  AND scope_type = 'system'
  AND scope_id = 0;
```

鍒犻櫎鍚庢彁浜ゅ簲鎭㈠銆?
---

## 浜屽崄鍥涖€佸父瑙侀棶棰?
### 24.1 deny 涓嶇敓鏁?
鎺掓煡锛?
```text
1. 鐢ㄦ埛鏄惁鏄?super_admin
2. principal_type 鏄惁鏄?user
3. principal_id 鏄惁姝ｇ‘
4. permission_code 鏄惁姝ｇ‘
5. scope_type / scope_id 鏄惁姝ｇ‘
6. expires_at 鏄惁宸茶繃鏈?7. 涓氬姟鏈嶅姟鏄惁鐪熺殑璋冪敤 RequireUserPermission
```

濡傛灉鐢ㄦ埛鏄?`super_admin`锛宒eny 涓嶇敓鏁堟槸璁捐濡傛銆?
---

### 24.2 鏅€?user 涓嶈兘鎻愪氦

鎺掓煡锛?
```text
1. permissions 鏄惁鏈?judge.submit
2. roles 鏄惁鏈?user
3. role_permissions 鏄惁鏈?user -> judge.submit
4. user_roles 鏄惁鏈?褰撳墠鐢ㄦ埛 -> user
5. judge-api 鏄惁浼犲叆 system:0
```

SQL锛?
```sql
SELECT r.name, rp.permission_code
FROM roles r
JOIN role_permissions rp ON rp.role_id = r.id
WHERE r.name = 'user'
ORDER BY rp.permission_code;
```

---

### 24.3 ErrForbidden 鐜板湪涓嶆槸 JSON

褰撳墠鍙兘杩斿洖锛?
```text
forbidden
```

杩欐槸涓嬩竴闃舵瑕佷慨鐨勭粺涓€閿欒鍝嶅簲闂銆?
鐩爣鍝嶅簲锛?
```json
{
  "code": 40301,
  "msg": "forbidden"
}
```

杩欏睘浜?HTTP 閿欒鍖呰锛屼笉灞炰簬 Permission Core 鍒ゆ柇妯″瀷鏈韩銆?
---

### 24.4 璧勬簮缁ф壙涓嶇敓鏁?
鎺掓煡锛?
```text
1. resource_edges 鏄惁鍐欏叆
2. parent / child 鏄惁鍐欏弽
3. relation 鏄惁绗﹀悎鏌ヨ閫昏緫
4. 鏉冮檺妫€鏌ユ槸鍚︽敹闆嗙埗绾?scope
5. role_bindings 鏄惁缁戝畾鍦ㄧ埗绾?scope 涓?```

渚嬪锛?
```text
contest:5 -> problem:7
```

搴旇〃绀猴細

```text
contest:5 contains problem:7
```

涓嶈鍐欏弽銆?
---

### 24.5 role_permissions 涓轰粈涔堜笉甯?scope

鍥犱负瑙掕壊鏄兘鍔涙ā鏉裤€?
渚嬪锛?
```text
contest_manager
```

瑙掕壊鏈韩琛ㄧず锛?
```text
鎷ユ湁绠＄悊姣旇禌鐨勪竴缁勮兘鍔?```

鑷充簬鐢ㄦ埛鍦ㄥ摢涓瘮璧涗笂鎷ユ湁杩欎釜瑙掕壊锛岀敱锛?
```text
role_bindings
```

鍐冲畾銆?
濡傛灉 role_permissions 甯?scope锛屼細瀵艰嚧鍚屼竴涓鑹插湪涓嶅悓璧勬簮涓婇噸澶嶅畾涔夛紝妯″瀷浼氭贩涔便€?
---

## 浜屽崄浜斻€佸畨鍏ㄦ敞鎰忎簨椤?
### 25.1 榛樿鎷掔粷

Permission Core 蹇呴』鍧氭寔锛?
```text
榛樿鎷掔粷
```

娌℃湁鏄庣‘鎺堟潈灏变笉鍏佽銆?
涓嶈兘鍥犱负娌℃湁 deny 灏卞厑璁搞€?
---

### 25.2 deny 浼樺厛

鏅€氱敤鎴风殑 deny 搴旇鐩栵細

```text
user_roles
role_bindings
direct allow
```

浣嗕笉瑕嗙洊锛?
```text
super_admin
```

---

### 25.3 涓嶅湪瀹㈡埛绔垽鏂潈闄?
鍓嶇鍙互鏍规嵁鏉冮檺鏄剧ず鎴栭殣钘忔寜閽紝浣嗕笉鑳藉彧渚濊禆鍓嶇銆?
鍚庣涓氬姟鏈嶅姟蹇呴』璋冪敤 Permission Core銆?
---

### 25.4 涓嶅湪 Gateway 鍐欎笟鍔℃潈闄?
Gateway 涓嶅簲璇ョ‖缂栫爜鏉冮檺鐐广€?
鍚﹀垯鏂板妯″潡浼氫笉鏂慨鏀?Gateway銆?
---

### 25.5 鏉冮檺鍙樻洿搴斿璁?
鏈潵鎵€鏈夋潈闄愬彉鏇撮兘搴斿啓鍏ワ細

```text
permission_audit_logs
```

鍖呮嫭锛?
```text
缁戝畾瑙掕壊
鎾ら攢瑙掕壊
鐩存帴鎺堟潈
鐩存帴鎷掔粷
鍒犻櫎鎺堟潈
娣诲姞璧勬簮鍏崇郴
鍒犻櫎璧勬簮鍏崇郴
```

---

## 浜屽崄鍏€佸悗缁鍒?
Permission Core 鍚庣画闇€瑕佽ˉ锛?
```text
缁熶竴 JSON 閿欒鍝嶅簲
permission-api
鏉冮檺绠＄悊鍓嶇
role revoke
permission revoke
resource edge remove
audit log query
鍒嗛〉鏌ヨ
鏉冮檺妯℃澘
妯″潡瀹夎鏃惰嚜鍔ㄦ敞鍐屾潈闄愮偣
妯″潡鍗歌浇鏃舵潈闄愬鐞嗙瓥鐣?scope inheritance 缂撳瓨
鏉冮檺鍒ゆ柇缂撳瓨
```

鎺ㄨ崘寮€鍙戦『搴忥細

```text
1. 缁熶竴閿欒鍝嶅簲
2. problem-api 鎺ュ叆 Permission Core
3. 鍒涘缓 problem 鍚庤嚜鍔ㄧ粦瀹?problem_owner
4. contest-api 鎺ュ叆 Permission Core
5. 鍒涘缓 contest 鍚庤嚜鍔ㄧ粦瀹?contest_owner
6. 鍐欏叆 contest -> problem resource_edges
7. permission-api
8. 鏉冮檺绠＄悊 UI
9. module-registry 鑷姩娉ㄥ唽 permission / resource_type
```

---

## 浜屽崄涓冦€佸綋鍓嶇粨璁?
Permission Core 褰撳墠宸茬粡瀹屾垚 OJOS 浠庣畝鍗曡鑹茬郴缁熷埌瀹屾暣璧勬簮绾ф潈闄愮郴缁熺殑鍩虹鍗囩骇銆?
褰撳墠妯″瀷鏀寔锛?
```text
principal_type / principal_id
scope_type / scope_id
system:0
type:0
resource_edges
allow / deny
super_admin
鍏ㄥ眬 user_roles
璧勬簮绾?role_bindings
role_permissions
permission_assignments
permission_audit_logs
```

瀹冨凡缁忕湡瀹炴帴鍏ワ細

```text
judge-api POST /judge/submissions
```

骞堕€氳繃锛?
```text
鏅€?user allow
鐩存帴 deny
鍒犻櫎 deny 鎭㈠
```

瀹屾垚楠岃瘉銆?
鍚庣画 OJOS 鐨勬墍鏈夋牳蹇冩ā鍧楅兘搴旇鎺ュ叆 Permission Core锛屽寘鎷細

```text
Problem Core
Dataset Core
Contest Core
Scoreboard Core
Balloon
Print
Forum
Clarification
Module Registry
Launcher
```

褰撳墠 Permission Core 鐨勬纭畾浣嶆槸锛?
```text
骞冲彴鍐呮牳绾ф巿鏉冪郴缁?```

瀹冨簲璇ヤ繚鎸佺ǔ瀹氾紝涓嶉殢涓氬姟妯″潡鍙嶅閲嶆瀯銆?
鏂板妯″潡搴旈€氳繃娉ㄥ唽鏁版嵁鎵╁睍 Permission Core锛岃€屼笉鏄慨鏀?Permission Core 鐨勬牳蹇冭〃缁撴瀯銆?
