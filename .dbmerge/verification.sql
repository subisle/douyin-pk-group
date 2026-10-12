/*M!999999\- enable the sandbox mode */ 
-- MariaDB dump 10.19  Distrib 10.11.18-MariaDB, for debian-linux-gnu (x86_64)
--
-- Host: localhost    Database: verification
-- ------------------------------------------------------
-- Server version	10.11.18-MariaDB-0+deb12u1

/*!40101 SET @OLD_CHARACTER_SET_CLIENT=@@CHARACTER_SET_CLIENT */;
/*!40101 SET @OLD_CHARACTER_SET_RESULTS=@@CHARACTER_SET_RESULTS */;
/*!40101 SET @OLD_COLLATION_CONNECTION=@@COLLATION_CONNECTION */;
/*!40101 SET NAMES utf8mb4 */;
/*!40103 SET @OLD_TIME_ZONE=@@TIME_ZONE */;
/*!40103 SET TIME_ZONE='+00:00' */;
/*!40014 SET @OLD_UNIQUE_CHECKS=@@UNIQUE_CHECKS, UNIQUE_CHECKS=0 */;
/*!40014 SET @OLD_FOREIGN_KEY_CHECKS=@@FOREIGN_KEY_CHECKS, FOREIGN_KEY_CHECKS=0 */;
/*!40101 SET @OLD_SQL_MODE=@@SQL_MODE, SQL_MODE='NO_AUTO_VALUE_ON_ZERO' */;
/*!40111 SET @OLD_SQL_NOTES=@@SQL_NOTES, SQL_NOTES=0 */;

--
-- Table structure for table "auth_grant"
--

DROP TABLE IF EXISTS "auth_grant";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "auth_grant" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "project_id" int(11) NOT NULL DEFAULT 0,
  "manager_id" int(11) NOT NULL DEFAULT 0,
  "request_id" varchar(96) DEFAULT NULL,
  "source" varchar(40) DEFAULT NULL,
  "account" varchar(254) DEFAULT NULL,
  "member_id" int(11) NOT NULL DEFAULT 0,
  "duration" bigint(20) NOT NULL DEFAULT 0,
  "points" int(11) NOT NULL DEFAULT 0,
  "package_kind" varchar(16) DEFAULT NULL,
  "mode" varchar(20) DEFAULT NULL,
  "mac" varchar(64) DEFAULT NULL,
  "device_info" longtext DEFAULT NULL,
  "remark" varchar(300) DEFAULT NULL,
  "status" varchar(20) DEFAULT NULL,
  "fail_reason" varchar(300) DEFAULT NULL,
  "before_time" bigint(20) NOT NULL DEFAULT 0,
  "after_time" bigint(20) NOT NULL DEFAULT 0,
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  KEY "auth_grant_project_id" ("project_id"),
  KEY "auth_grant_manager_id" ("manager_id"),
  KEY "auth_grant_request_id" ("request_id"),
  KEY "auth_grant_source" ("source"),
  KEY "auth_grant_account" ("account"),
  KEY "auth_grant_member_id" ("member_id"),
  KEY "auth_grant_mac" ("mac"),
  KEY "auth_grant_status" ("status"),
  KEY "auth_grant_create_time" ("create_time")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "auth_grant"
--

LOCK TABLES "auth_grant" WRITE;
/*!40000 ALTER TABLE "auth_grant" DISABLE KEYS */;
/*!40000 ALTER TABLE "auth_grant" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "cards"
--

DROP TABLE IF EXISTS "cards";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "cards" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "project_id" int(11) NOT NULL DEFAULT 0,
  "manager_id" int(11) NOT NULL DEFAULT 0,
  "title" varchar(28) DEFAULT NULL COMMENT '激活码名称',
  "price" decimal(12,2) NOT NULL DEFAULT 0.00 COMMENT '激活码定价',
  "key_prefix" varchar(4) DEFAULT NULL COMMENT '激活码前缀',
  "level_id" int(11) NOT NULL DEFAULT 0 COMMENT '关联会员套餐',
  "days" decimal(12,2) NOT NULL DEFAULT 0.00 COMMENT '天数',
  "points" int(11) NOT NULL DEFAULT 0 COMMENT '张数',
  "package_kind" varchar(16) DEFAULT NULL COMMENT '套餐类型 time/count/mixed',
  "feature_scopes_json" longtext DEFAULT NULL,
  "key_ext_attr" varchar(200) DEFAULT NULL COMMENT '激活码扩展属性',
  "tag" varchar(200) DEFAULT NULL COMMENT '标签',
  "is_lock" int(11) NOT NULL DEFAULT 0 COMMENT '0正常,1锁定',
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  KEY "cards_project_id" ("project_id"),
  KEY "cards_manager_id" ("manager_id"),
  KEY "cards_key_prefix" ("key_prefix"),
  KEY "cards_level_id" ("level_id"),
  KEY "cards_is_lock" ("is_lock"),
  KEY "cards_create_time" ("create_time")
) ENGINE=InnoDB AUTO_INCREMENT=2 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "cards"
--

LOCK TABLES "cards" WRITE;
/*!40000 ALTER TABLE "cards" DISABLE KEYS */;
INSERT INTO "cards" VALUES
(1,1,1,'YY狼音音效月卡30天',0.00,'AC',0,30.00,0,'time','','','yy-langyin-sfx',0,'2026-07-20 05:41:31');
/*!40000 ALTER TABLE "cards" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "daily_quota_counter"
--

DROP TABLE IF EXISTS "daily_quota_counter";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "daily_quota_counter" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "counter_key" varchar(96) NOT NULL DEFAULT '',
  "project_id" int(11) NOT NULL DEFAULT 0,
  "kind" varchar(16) NOT NULL DEFAULT '',
  "day" varchar(8) NOT NULL DEFAULT '',
  "used" int(11) NOT NULL DEFAULT 0,
  PRIMARY KEY ("id"),
  UNIQUE KEY "counter_key" ("counter_key"),
  KEY "daily_quota_counter_project_id" ("project_id"),
  KEY "daily_quota_counter_kind" ("kind"),
  KEY "daily_quota_counter_day" ("day")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "daily_quota_counter"
--

LOCK TABLES "daily_quota_counter" WRITE;
/*!40000 ALTER TABLE "daily_quota_counter" DISABLE KEYS */;
/*!40000 ALTER TABLE "daily_quota_counter" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "email_delivery"
--

DROP TABLE IF EXISTS "email_delivery";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "email_delivery" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "message_id" varchar(43) NOT NULL DEFAULT '',
  "request_key" varchar(43) NOT NULL DEFAULT '',
  "request_id" varchar(64) NOT NULL DEFAULT '',
  "project_id" int(11) NOT NULL DEFAULT 0,
  "manager_id" int(11) NOT NULL DEFAULT 0,
  "external_client_id" int(11) NOT NULL DEFAULT 0,
  "client_id" varchar(43) NOT NULL DEFAULT '',
  "recipient_hash" varchar(43) NOT NULL DEFAULT '',
  "template_id" varchar(64) NOT NULL DEFAULT '',
  "payload_box" longtext NOT NULL,
  "status" varchar(16) NOT NULL DEFAULT '',
  "attempts" int(11) NOT NULL DEFAULT 0,
  "next_attempt_at" bigint(20) NOT NULL DEFAULT 0,
  "lease_until" bigint(20) NOT NULL DEFAULT 0,
  "lease_token" varchar(43) DEFAULT NULL,
  "last_error" varchar(64) DEFAULT NULL,
  "sent_at" bigint(20) NOT NULL DEFAULT 0,
  "create_time" datetime NOT NULL,
  "update_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  UNIQUE KEY "message_id" ("message_id"),
  UNIQUE KEY "request_key" ("request_key"),
  KEY "email_delivery_request_id" ("request_id"),
  KEY "email_delivery_project_id" ("project_id"),
  KEY "email_delivery_manager_id" ("manager_id"),
  KEY "email_delivery_external_client_id" ("external_client_id"),
  KEY "email_delivery_client_id" ("client_id"),
  KEY "email_delivery_recipient_hash" ("recipient_hash"),
  KEY "email_delivery_template_id" ("template_id"),
  KEY "email_delivery_status" ("status"),
  KEY "email_delivery_next_attempt_at" ("next_attempt_at"),
  KEY "email_delivery_lease_until" ("lease_until"),
  KEY "email_delivery_sent_at" ("sent_at"),
  KEY "email_delivery_create_time" ("create_time"),
  KEY "email_delivery_update_time" ("update_time")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "email_delivery"
--

LOCK TABLES "email_delivery" WRITE;
/*!40000 ALTER TABLE "email_delivery" DISABLE KEYS */;
/*!40000 ALTER TABLE "email_delivery" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "email_domain"
--

DROP TABLE IF EXISTS "email_domain";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "email_domain" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "manager_id" int(11) NOT NULL DEFAULT 0,
  "domain_key" varchar(64) DEFAULT NULL,
  "domain" varchar(253) NOT NULL,
  "remark" varchar(200) DEFAULT NULL,
  "is_active" int(11) NOT NULL DEFAULT 1 COMMENT '0禁用,1启用',
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  UNIQUE KEY "domain_key" ("domain_key"),
  KEY "email_domain_manager_id" ("manager_id"),
  KEY "email_domain_domain" ("domain"),
  KEY "email_domain_is_active" ("is_active"),
  KEY "email_domain_create_time" ("create_time")
) ENGINE=InnoDB AUTO_INCREMENT=4 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "email_domain"
--

LOCK TABLES "email_domain" WRITE;
/*!40000 ALTER TABLE "email_domain" DISABLE KEYS */;
INSERT INTO "email_domain" VALUES
(1,1,'23864e42e125f0d944605561c0ce980336e8e02c5d62d9d8404f5b49c3c172de','gmail.com','anticrack',0,'2026-07-20 05:41:31'),
(2,1,'23d1a792e04275cc891809684d32b5ed57601045c2ea8bc92983c2200a0acd87','163.com','anticrack',0,'2026-07-20 05:41:31'),
(3,1,'d9980f31af00398fa99111350b81b4acc3d0ea81d1c4fe28f32f883f8030bb73','qq.com','西瓜狗QQ邮箱',1,'2026-07-20 06:08:30');
/*!40000 ALTER TABLE "email_domain" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "external_api_client"
--

DROP TABLE IF EXISTS "external_api_client";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "external_api_client" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "client_id" varchar(43) NOT NULL DEFAULT '',
  "project_id" int(11) NOT NULL DEFAULT 0,
  "manager_id" int(11) NOT NULL DEFAULT 0,
  "name" varchar(80) NOT NULL DEFAULT '',
  "public_key" varchar(43) NOT NULL DEFAULT '',
  "scopes_json" longtext NOT NULL,
  "ip_allowlist" longtext NOT NULL,
  "status" int(11) NOT NULL DEFAULT 0,
  "expires_at" bigint(20) NOT NULL DEFAULT 0,
  "key_version" bigint(20) NOT NULL DEFAULT 0,
  "unbind_deduct_seconds" bigint(20) NOT NULL DEFAULT 0,
  "last_used_at" bigint(20) NOT NULL DEFAULT 0,
  "create_time" datetime NOT NULL,
  "update_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  UNIQUE KEY "client_id" ("client_id"),
  UNIQUE KEY "public_key" ("public_key"),
  KEY "external_api_client_project_id" ("project_id"),
  KEY "external_api_client_manager_id" ("manager_id"),
  KEY "external_api_client_status" ("status"),
  KEY "external_api_client_expires_at" ("expires_at"),
  KEY "external_api_client_key_version" ("key_version"),
  KEY "external_api_client_last_used_at" ("last_used_at"),
  KEY "external_api_client_create_time" ("create_time"),
  KEY "external_api_client_update_time" ("update_time")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "external_api_client"
--

LOCK TABLES "external_api_client" WRITE;
/*!40000 ALTER TABLE "external_api_client" DISABLE KEYS */;
/*!40000 ALTER TABLE "external_api_client" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "external_api_client_audit"
--

DROP TABLE IF EXISTS "external_api_client_audit";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "external_api_client_audit" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "project_id" int(11) NOT NULL DEFAULT 0,
  "external_client_id" int(11) NOT NULL DEFAULT 0,
  "client_id" varchar(43) NOT NULL DEFAULT '',
  "actor_manager_id" int(11) NOT NULL DEFAULT 0,
  "action" varchar(16) NOT NULL DEFAULT '',
  "previous_status" int(11) NOT NULL DEFAULT 0,
  "new_status" int(11) NOT NULL DEFAULT 0,
  "previous_version" bigint(20) NOT NULL DEFAULT 0,
  "new_version" bigint(20) NOT NULL DEFAULT 0,
  "reason" varchar(240) NOT NULL DEFAULT '',
  "remote_ip" varchar(64) DEFAULT NULL,
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  KEY "external_api_client_audit_project_id" ("project_id"),
  KEY "external_api_client_audit_external_client_id" ("external_client_id"),
  KEY "external_api_client_audit_client_id" ("client_id"),
  KEY "external_api_client_audit_actor_manager_id" ("actor_manager_id"),
  KEY "external_api_client_audit_action" ("action"),
  KEY "external_api_client_audit_create_time" ("create_time")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "external_api_client_audit"
--

LOCK TABLES "external_api_client_audit" WRITE;
/*!40000 ALTER TABLE "external_api_client_audit" DISABLE KEYS */;
/*!40000 ALTER TABLE "external_api_client_audit" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "external_api_request"
--

DROP TABLE IF EXISTS "external_api_request";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "external_api_request" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "request_key" varchar(43) NOT NULL DEFAULT '',
  "nonce_key" varchar(43) NOT NULL DEFAULT '',
  "external_client_id" int(11) NOT NULL DEFAULT 0,
  "client_id" varchar(43) NOT NULL DEFAULT '',
  "project_id" int(11) NOT NULL DEFAULT 0,
  "member_id" int(11) NOT NULL DEFAULT 0,
  "request_id" varchar(64) NOT NULL DEFAULT '',
  "request_hash" varchar(43) NOT NULL DEFAULT '',
  "account_hash" varchar(43) DEFAULT NULL,
  "action" varchar(64) NOT NULL DEFAULT '',
  "status" varchar(16) NOT NULL DEFAULT '',
  "outcome_code" varchar(64) NOT NULL DEFAULT '',
  "http_status" int(11) NOT NULL DEFAULT 0,
  "response_box" longtext NOT NULL,
  "remote_ip" varchar(64) DEFAULT NULL,
  "client_key_version" bigint(20) NOT NULL DEFAULT 0,
  "completed_at" bigint(20) NOT NULL DEFAULT 0,
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  UNIQUE KEY "request_key" ("request_key"),
  UNIQUE KEY "nonce_key" ("nonce_key"),
  KEY "external_api_request_external_client_id" ("external_client_id"),
  KEY "external_api_request_client_id" ("client_id"),
  KEY "external_api_request_project_id" ("project_id"),
  KEY "external_api_request_member_id" ("member_id"),
  KEY "external_api_request_request_id" ("request_id"),
  KEY "external_api_request_account_hash" ("account_hash"),
  KEY "external_api_request_action" ("action"),
  KEY "external_api_request_status" ("status"),
  KEY "external_api_request_outcome_code" ("outcome_code"),
  KEY "external_api_request_completed_at" ("completed_at"),
  KEY "external_api_request_create_time" ("create_time")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "external_api_request"
--

LOCK TABLES "external_api_request" WRITE;
/*!40000 ALTER TABLE "external_api_request" DISABLE KEYS */;
/*!40000 ALTER TABLE "external_api_request" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "external_authorization_state"
--

DROP TABLE IF EXISTS "external_authorization_state";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "external_authorization_state" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "state_key" varchar(64) NOT NULL DEFAULT '',
  "project_id" int(11) NOT NULL DEFAULT 0,
  "member_id" int(11) NOT NULL DEFAULT 0,
  "status" varchar(16) NOT NULL DEFAULT '',
  "version" bigint(20) NOT NULL DEFAULT 0,
  "create_time" datetime NOT NULL,
  "update_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  UNIQUE KEY "state_key" ("state_key"),
  KEY "external_authorization_state_project_id" ("project_id"),
  KEY "external_authorization_state_member_id" ("member_id"),
  KEY "external_authorization_state_status" ("status"),
  KEY "external_authorization_state_version" ("version"),
  KEY "external_authorization_state_create_time" ("create_time"),
  KEY "external_authorization_state_update_time" ("update_time")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "external_authorization_state"
--

LOCK TABLES "external_authorization_state" WRITE;
/*!40000 ALTER TABLE "external_authorization_state" DISABLE KEYS */;
/*!40000 ALTER TABLE "external_authorization_state" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "keys"
--

DROP TABLE IF EXISTS "keys";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "keys" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "project_id" int(11) NOT NULL DEFAULT 0,
  "manager_id" int(11) NOT NULL DEFAULT 0,
  "long_keys" varchar(32) DEFAULT NULL COMMENT '激活码卡密',
  "cards_id" int(11) NOT NULL DEFAULT 0 COMMENT '关联激活码类型ID',
  "level_id" int(11) NOT NULL DEFAULT 0 COMMENT '套餐VIP级别,0普通',
  "order_id" int(11) NOT NULL DEFAULT 0 COMMENT '关联激活码订单ID',
  "member" varchar(60) DEFAULT NULL COMMENT '关联用户',
  "member_id" int(11) NOT NULL DEFAULT 0 COMMENT '关联用户ID',
  "use_time" int(11) NOT NULL DEFAULT 0 COMMENT '使用时间',
  "tag" varchar(200) DEFAULT NULL COMMENT '标签',
  "is_lock" int(11) NOT NULL DEFAULT 0 COMMENT '0正常,1锁定',
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  UNIQUE KEY "long_keys" ("long_keys"),
  UNIQUE KEY "security_uq_keys_long_keys" ("long_keys"),
  KEY "keys_project_id" ("project_id"),
  KEY "keys_manager_id" ("manager_id"),
  KEY "keys_cards_id" ("cards_id"),
  KEY "keys_level_id" ("level_id"),
  KEY "keys_order_id" ("order_id"),
  KEY "keys_member_id" ("member_id"),
  KEY "keys_use_time" ("use_time"),
  KEY "keys_is_lock" ("is_lock"),
  KEY "keys_create_time" ("create_time")
) ENGINE=InnoDB AUTO_INCREMENT=11 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "keys"
--

LOCK TABLES "keys" WRITE;
/*!40000 ALTER TABLE "keys" DISABLE KEYS */;
INSERT INTO "keys" VALUES
(1,1,1,'AClY4syLGxvCa8s2',1,0,1,'e_8166fc5aaaed5f516925b36313c82f',1,1784612934,'anticrack',0,'2026-07-20 05:41:31'),
(2,1,1,'ACShFWleSieb9kwW',1,0,1,'',0,0,'anticrack',0,'2026-07-20 05:41:31'),
(3,1,1,'ACvRNzrmBnsDwmAL',1,0,1,'',0,0,'anticrack',0,'2026-07-20 05:41:31'),
(4,1,1,'ACzhYm5wTbUQ2L1R',1,0,1,'',0,0,'anticrack',0,'2026-07-20 05:41:31'),
(5,1,1,'AC7kFughRUgycQYf',1,0,1,'',0,0,'anticrack',0,'2026-07-20 05:41:31'),
(6,1,1,'AC1KroYHn7k7ZqwS',1,0,1,'',0,0,'anticrack',0,'2026-07-20 05:41:31'),
(7,1,1,'ACkv3FU4kgF9JLGK',1,0,1,'',0,0,'anticrack',0,'2026-07-20 05:41:31'),
(8,1,1,'ACZzP3zeTUsWtA8f',1,0,1,'',0,0,'anticrack',0,'2026-07-20 05:41:31'),
(9,1,1,'ACLPvHlQfQWZ5YyZ',1,0,1,'',0,0,'anticrack',0,'2026-07-20 05:41:31'),
(10,1,1,'ACEFAhjRPyoa2Hpx',1,0,1,'e_8166fc5aaaed5f516925b36313c82f',1,1784613750,'anticrack',0,'2026-07-20 05:41:31');
/*!40000 ALTER TABLE "keys" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "level"
--

DROP TABLE IF EXISTS "level";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "level" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "project_id" int(11) NOT NULL DEFAULT 0,
  "manager_id" int(11) NOT NULL DEFAULT 0,
  "title" varchar(28) DEFAULT NULL COMMENT '套餐名称',
  "price" decimal(12,2) NOT NULL DEFAULT 0.00 COMMENT '套餐定价',
  "level" int(11) NOT NULL DEFAULT 0 COMMENT '套餐级别',
  "cover_type" int(11) NOT NULL DEFAULT 0 COMMENT '0叠加,1覆盖',
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  KEY "level_project_id" ("project_id"),
  KEY "level_manager_id" ("manager_id"),
  KEY "level_level" ("level"),
  KEY "level_cover_type" ("cover_type"),
  KEY "level_create_time" ("create_time")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "level"
--

LOCK TABLES "level" WRITE;
/*!40000 ALTER TABLE "level" DISABLE KEYS */;
/*!40000 ALTER TABLE "level" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "manager"
--

DROP TABLE IF EXISTS "manager";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "manager" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "email" varchar(254) DEFAULT NULL,
  "user" varchar(60) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin NOT NULL,
  "pwd" varchar(60) NOT NULL DEFAULT '' COMMENT '密码',
  "level" int(11) NOT NULL DEFAULT 0 COMMENT '等级',
  "avatar" varchar(240) DEFAULT NULL COMMENT '头像',
  "scope" int(11) NOT NULL DEFAULT 0 COMMENT '积分',
  "pid" int(11) NOT NULL DEFAULT 0 COMMENT '关联上级ID',
  "invite_id" int(11) NOT NULL DEFAULT 0 COMMENT '邀请人ID',
  "invite" varchar(6) DEFAULT NULL COMMENT '邀请码',
  "money" decimal(12,2) NOT NULL DEFAULT 0.00 COMMENT '关联账户余额',
  "login_time" int(11) NOT NULL DEFAULT 0 COMMENT '登录时间',
  "login_ip" varchar(64) DEFAULT NULL,
  "is_lock" int(11) NOT NULL DEFAULT 0 COMMENT '0正常,1锁定',
  "is_del" int(11) NOT NULL DEFAULT 0 COMMENT '0正常,1删除',
  "power_id" int(11) NOT NULL DEFAULT 0 COMMENT '关联权限ID',
  "session_version" bigint(20) NOT NULL DEFAULT 1 COMMENT '会话版本',
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  UNIQUE KEY "security_uq_manager_user" ("user"),
  KEY "manager_email" ("email"),
  KEY "manager_user" ("user"),
  KEY "manager_pid" ("pid"),
  KEY "manager_invite_id" ("invite_id"),
  KEY "manager_login_time" ("login_time"),
  KEY "manager_is_lock" ("is_lock"),
  KEY "manager_is_del" ("is_del"),
  KEY "manager_create_time" ("create_time")
) ENGINE=InnoDB AUTO_INCREMENT=2 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "manager"
--

LOCK TABLES "manager" WRITE;
/*!40000 ALTER TABLE "manager" DISABLE KEYS */;
INSERT INTO "manager" VALUES
(1,'','vadmin','$2a$10$KG0qV54oYNms4XzOFr0KCuKUFTRqmQjAAX3ZbPZkk8nxXOgWUAnmu',0,'',0,0,0,'',0.00,0,'',0,0,0,1,'2026-07-20 05:18:36');
/*!40000 ALTER TABLE "manager" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "manager_cards"
--

DROP TABLE IF EXISTS "manager_cards";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "manager_cards" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "project_id" int(11) NOT NULL DEFAULT 0,
  "manager_id" int(11) NOT NULL DEFAULT 0,
  "cards_id" int(11) NOT NULL DEFAULT 0 COMMENT '关联激活码类型ID',
  "price" decimal(12,2) NOT NULL DEFAULT 0.00 COMMENT '激活码定价',
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  KEY "manager_cards_project_id" ("project_id"),
  KEY "manager_cards_manager_id" ("manager_id"),
  KEY "manager_cards_cards_id" ("cards_id"),
  KEY "manager_cards_create_time" ("create_time")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "manager_cards"
--

LOCK TABLES "manager_cards" WRITE;
/*!40000 ALTER TABLE "manager_cards" DISABLE KEYS */;
/*!40000 ALTER TABLE "manager_cards" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "member"
--

DROP TABLE IF EXISTS "member";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "member" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "manager_id" int(11) NOT NULL DEFAULT 0,
  "project_id" int(11) NOT NULL DEFAULT 0,
  "parent_id" int(11) NOT NULL DEFAULT 0,
  "email" varchar(254) DEFAULT NULL,
  "email_verified" int(11) NOT NULL DEFAULT 0 COMMENT '0未验证,1已验证',
  "email_verify_at" bigint(20) NOT NULL DEFAULT 0 COMMENT '邮箱验证时间',
  "name" varchar(64) NOT NULL DEFAULT '',
  "nick_name" varchar(64) DEFAULT NULL,
  "password" varchar(80) DEFAULT NULL,
  "safe_password" varchar(80) DEFAULT NULL,
  "money" decimal(12,2) NOT NULL DEFAULT 0.00,
  "days" decimal(12,2) NOT NULL DEFAULT 0.00,
  "points" int(11) NOT NULL DEFAULT 0,
  "time_unlimited" int(11) NOT NULL DEFAULT 0 COMMENT '0限时,1不限时',
  "points_unlimited" int(11) NOT NULL DEFAULT 0 COMMENT '0限张,1不限张',
  "mac" varchar(32) DEFAULT NULL,
  "phone" bigint(20) NOT NULL DEFAULT 0,
  "scope" int(11) NOT NULL DEFAULT 0,
  "key_ext_attr" varchar(200) DEFAULT NULL,
  "tag" varchar(200) DEFAULT NULL,
  "active_time" bigint(20) NOT NULL DEFAULT 0,
  "end_time" bigint(20) NOT NULL DEFAULT 0,
  "pause_time" bigint(20) NOT NULL DEFAULT 0,
  "last_login_time" bigint(20) NOT NULL DEFAULT 0,
  "last_login_ip" varchar(64) DEFAULT NULL,
  "is_lock" int(11) NOT NULL DEFAULT 0 COMMENT '0正常,1锁定',
  "risk_freeze_until" bigint(20) NOT NULL DEFAULT 0,
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  KEY "member_manager_id" ("manager_id"),
  KEY "member_project_id" ("project_id"),
  KEY "member_parent_id" ("parent_id"),
  KEY "member_email" ("email"),
  KEY "member_email_verified" ("email_verified"),
  KEY "member_email_verify_at" ("email_verify_at"),
  KEY "member_name" ("name"),
  KEY "member_nick_name" ("nick_name"),
  KEY "member_time_unlimited" ("time_unlimited"),
  KEY "member_points_unlimited" ("points_unlimited"),
  KEY "member_active_time" ("active_time"),
  KEY "member_end_time" ("end_time"),
  KEY "member_pause_time" ("pause_time"),
  KEY "member_last_login_time" ("last_login_time"),
  KEY "member_last_login_ip" ("last_login_ip"),
  KEY "member_is_lock" ("is_lock"),
  KEY "member_risk_freeze_until" ("risk_freeze_until"),
  KEY "member_create_time" ("create_time")
) ENGINE=InnoDB AUTO_INCREMENT=2 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "member"
--

LOCK TABLES "member" WRITE;
/*!40000 ALTER TABLE "member" DISABLE KEYS */;
INSERT INTO "member" VALUES
(1,1,1,0,'cszmy1@qq.com',1,1784612934,'e_8166fc5aaaed5f516925b36313c82f','cszmy1@qq.com','$2a$10$H.9mpbq66gIyMks2m/Zn9.0D5fEDpv/D2GGKoYpIrHt/9Cjgfxp6G','$2a$10$HU0343DzseNOyfRbS.La0.N7d1rNgvy/wUmNZhEgEig2wlFFykRM.',0.00,59.99,0,0,1,'',0,0,'','anticrack',1784612934,1789796934,0,1788951929,'106.42.178.204',0,0,'2026-07-21 01:48:54');
/*!40000 ALTER TABLE "member" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "member_feature_grant"
--

DROP TABLE IF EXISTS "member_feature_grant";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "member_feature_grant" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "grant_key" varchar(64) NOT NULL DEFAULT '',
  "project_id" int(11) NOT NULL DEFAULT 0,
  "member_id" int(11) NOT NULL DEFAULT 0,
  "operation" varchar(64) NOT NULL DEFAULT '',
  "source_card_id" int(11) NOT NULL DEFAULT 0,
  "source_key_id" int(11) NOT NULL DEFAULT 0,
  "granted_at" bigint(20) NOT NULL DEFAULT 0,
  "expires_at" bigint(20) NOT NULL DEFAULT 0,
  "status" int(11) NOT NULL DEFAULT 1,
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  UNIQUE KEY "grant_key" ("grant_key"),
  KEY "member_feature_grant_project_id" ("project_id"),
  KEY "member_feature_grant_member_id" ("member_id"),
  KEY "member_feature_grant_operation" ("operation"),
  KEY "member_feature_grant_source_card_id" ("source_card_id"),
  KEY "member_feature_grant_source_key_id" ("source_key_id"),
  KEY "member_feature_grant_granted_at" ("granted_at"),
  KEY "member_feature_grant_expires_at" ("expires_at"),
  KEY "member_feature_grant_status" ("status"),
  KEY "member_feature_grant_create_time" ("create_time"),
  KEY "member_feature_grant_project_id_member_id_operation_status" ("project_id","member_id","operation","status")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "member_feature_grant"
--

LOCK TABLES "member_feature_grant" WRITE;
/*!40000 ALTER TABLE "member_feature_grant" DISABLE KEYS */;
/*!40000 ALTER TABLE "member_feature_grant" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "member_free"
--

DROP TABLE IF EXISTS "member_free";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "member_free" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "manager_id" int(11) NOT NULL DEFAULT 0,
  "project_id" int(11) NOT NULL DEFAULT 0,
  "member" varchar(60) DEFAULT NULL,
  "member_id" int(11) NOT NULL DEFAULT 0,
  "mac" varchar(32) DEFAULT NULL,
  "last_time" int(11) NOT NULL DEFAULT 0,
  "days" decimal(12,2) NOT NULL DEFAULT 0.00,
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  KEY "member_free_manager_id" ("manager_id"),
  KEY "member_free_project_id" ("project_id"),
  KEY "member_free_member_id" ("member_id"),
  KEY "member_free_last_time" ("last_time"),
  KEY "member_free_create_time" ("create_time")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "member_free"
--

LOCK TABLES "member_free" WRITE;
/*!40000 ALTER TABLE "member_free" DISABLE KEYS */;
/*!40000 ALTER TABLE "member_free" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "member_identity"
--

DROP TABLE IF EXISTS "member_identity";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "member_identity" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "identity_key" varchar(64) NOT NULL DEFAULT '',
  "project_id" int(11) NOT NULL DEFAULT 0,
  "kind" varchar(12) NOT NULL DEFAULT '',
  "value" varchar(254) NOT NULL,
  "member_id" int(11) NOT NULL DEFAULT 0,
  PRIMARY KEY ("id"),
  UNIQUE KEY "identity_key" ("identity_key"),
  KEY "member_identity_project_id" ("project_id"),
  KEY "member_identity_kind" ("kind"),
  KEY "member_identity_member_id" ("member_id")
) ENGINE=InnoDB AUTO_INCREMENT=3 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "member_identity"
--

LOCK TABLES "member_identity" WRITE;
/*!40000 ALTER TABLE "member_identity" DISABLE KEYS */;
INSERT INTO "member_identity" VALUES
(1,'06b9b31789b79c854cfe638b04a8ccbff921e165251ced590894193728edb2af',1,'name','e_8166fc5aaaed5f516925b36313c82f',1),
(2,'52a951604c93b276f89e7ee9c54395ada4bbc6618b06aa9d3f42fac2face97f2',1,'email','cszmy1@qq.com',1);
/*!40000 ALTER TABLE "member_identity" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "member_login"
--

DROP TABLE IF EXISTS "member_login";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "member_login" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "manager_id" int(11) NOT NULL DEFAULT 0,
  "project_id" int(11) NOT NULL DEFAULT 0,
  "member" varchar(254) DEFAULT NULL,
  "member_id" int(11) NOT NULL DEFAULT 0,
  "mac" varchar(32) DEFAULT NULL,
  "last_login_time" int(11) NOT NULL DEFAULT 0,
  "last_login_ip" varchar(64) DEFAULT NULL,
  "is_online" int(11) NOT NULL DEFAULT 0 COMMENT '0 在线 1 下线',
  "online_id" varchar(56) DEFAULT NULL COMMENT '单用户唯一在线id',
  "type" int(11) NOT NULL DEFAULT 0 COMMENT '0登录,1解绑,2下线,3心跳',
  "login_type" int(11) NOT NULL DEFAULT 0 COMMENT '0普通登录,1外部身份A,2外部身份B,3外部身份C',
  "open_id" varchar(30) DEFAULT NULL,
  "access_token" varchar(50) DEFAULT NULL,
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  UNIQUE KEY "online_id" ("online_id"),
  KEY "member_login_manager_id" ("manager_id"),
  KEY "member_login_project_id" ("project_id"),
  KEY "member_login_member_id" ("member_id"),
  KEY "member_login_last_login_time" ("last_login_time"),
  KEY "member_login_last_login_ip" ("last_login_ip"),
  KEY "member_login_is_online" ("is_online"),
  KEY "member_login_type" ("type"),
  KEY "member_login_login_type" ("login_type"),
  KEY "member_login_create_time" ("create_time")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "member_login"
--

LOCK TABLES "member_login" WRITE;
/*!40000 ALTER TABLE "member_login" DISABLE KEYS */;
/*!40000 ALTER TABLE "member_login" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "operation_claim"
--

DROP TABLE IF EXISTS "operation_claim";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "operation_claim" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "claim_key" varchar(96) NOT NULL DEFAULT '',
  "project_id" int(11) NOT NULL DEFAULT 0,
  "action" varchar(32) NOT NULL DEFAULT '',
  "request_id" varchar(96) NOT NULL DEFAULT '',
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  UNIQUE KEY "claim_key" ("claim_key"),
  KEY "operation_claim_project_id" ("project_id"),
  KEY "operation_claim_action" ("action"),
  KEY "operation_claim_request_id" ("request_id"),
  KEY "operation_claim_create_time" ("create_time")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "operation_claim"
--

LOCK TABLES "operation_claim" WRITE;
/*!40000 ALTER TABLE "operation_claim" DISABLE KEYS */;
/*!40000 ALTER TABLE "operation_claim" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "order"
--

DROP TABLE IF EXISTS "order";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "order" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "project_id" int(11) NOT NULL DEFAULT 0,
  "manager_id" int(11) NOT NULL DEFAULT 0,
  "cards_id" int(11) NOT NULL DEFAULT 0 COMMENT '关联激活码类型ID',
  "count" int(11) NOT NULL DEFAULT 0 COMMENT '激活码个数',
  "price" decimal(12,2) NOT NULL DEFAULT 0.00 COMMENT '总金额',
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  KEY "order_project_id" ("project_id"),
  KEY "order_manager_id" ("manager_id"),
  KEY "order_cards_id" ("cards_id")
) ENGINE=InnoDB AUTO_INCREMENT=2 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "order"
--

LOCK TABLES "order" WRITE;
/*!40000 ALTER TABLE "order" DISABLE KEYS */;
INSERT INTO "order" VALUES
(1,1,1,1,10,0.00,'2026-07-20 05:41:31');
/*!40000 ALTER TABLE "order" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "point_ledger"
--

DROP TABLE IF EXISTS "point_ledger";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "point_ledger" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "project_id" int(11) NOT NULL DEFAULT 0,
  "manager_id" int(11) NOT NULL DEFAULT 0,
  "member_id" int(11) NOT NULL DEFAULT 0,
  "account" varchar(254) DEFAULT NULL,
  "request_id" varchar(96) DEFAULT NULL,
  "biz_id" varchar(96) DEFAULT NULL,
  "action" varchar(20) DEFAULT NULL,
  "change" int(11) NOT NULL DEFAULT 0,
  "before_points" int(11) NOT NULL DEFAULT 0,
  "after_points" int(11) NOT NULL DEFAULT 0,
  "source" varchar(40) DEFAULT NULL,
  "remark" varchar(300) DEFAULT NULL,
  "status" varchar(20) DEFAULT NULL,
  "fail_reason" varchar(300) DEFAULT NULL,
  "end_time" bigint(20) NOT NULL DEFAULT 0,
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  KEY "point_ledger_project_id" ("project_id"),
  KEY "point_ledger_manager_id" ("manager_id"),
  KEY "point_ledger_member_id" ("member_id"),
  KEY "point_ledger_account" ("account"),
  KEY "point_ledger_request_id" ("request_id"),
  KEY "point_ledger_biz_id" ("biz_id"),
  KEY "point_ledger_action" ("action"),
  KEY "point_ledger_source" ("source"),
  KEY "point_ledger_status" ("status"),
  KEY "point_ledger_create_time" ("create_time")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "point_ledger"
--

LOCK TABLES "point_ledger" WRITE;
/*!40000 ALTER TABLE "point_ledger" DISABLE KEYS */;
/*!40000 ALTER TABLE "point_ledger" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "project"
--

DROP TABLE IF EXISTS "project";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "project" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "manager_id" int(11) NOT NULL DEFAULT 0,
  "name" varchar(40) NOT NULL DEFAULT '',
  "app_key" varchar(32) CHARACTER SET ascii COLLATE ascii_bin NOT NULL,
  "secret_key" varchar(32) NOT NULL DEFAULT '',
  "type" int(11) NOT NULL DEFAULT 0 COMMENT '0单码,1账号',
  "status_type" int(11) NOT NULL DEFAULT 0 COMMENT '0收费,1停止,2免费',
  "account_api_enabled" int(11) NOT NULL DEFAULT 0 COMMENT '0关闭公开账号API,1开启',
  "account_mode" int(11) NOT NULL DEFAULT 0 COMMENT '0微信wxid或邮箱,1仅邮箱账号',
  "email_verify" int(11) NOT NULL DEFAULT 0 COMMENT '0不强制邮箱验证,1需要邮箱验证',
  "device_fp_mode" int(11) NOT NULL DEFAULT 0,
  "build_hash_mode" int(11) NOT NULL DEFAULT 0 COMMENT '0不校验,1登录记录,2强制匹配发布表',
  "resource_alias_mode" int(11) NOT NULL DEFAULT 0 COMMENT '0关闭,1强制别名',
  "decoy_enabled" int(11) NOT NULL DEFAULT 0 COMMENT '0关闭,1高风险可发decoy',
  "public_key" longtext DEFAULT NULL COMMENT 'RSA公钥',
  "private_key" longtext DEFAULT NULL COMMENT 'RSA私钥',
  "key_a" varchar(16) DEFAULT NULL COMMENT 'AES加密密匙A',
  "key_b" varchar(16) DEFAULT NULL COMMENT 'AES加密密匙B',
  "sign" int(11) NOT NULL DEFAULT 0,
  "encrypt" int(11) NOT NULL DEFAULT 0,
  "login_type" int(11) NOT NULL DEFAULT 0,
  "gift_type" int(11) NOT NULL DEFAULT 0,
  "free_type" int(11) NOT NULL DEFAULT 0,
  "free_range_start" varchar(16) DEFAULT NULL COMMENT '试用开始时间',
  "free_range_end" varchar(16) DEFAULT NULL COMMENT '试用开始时间',
  "free_times" int(11) NOT NULL DEFAULT 0,
  "free_gift_times" double NOT NULL DEFAULT 0,
  "notice" longtext DEFAULT NULL COMMENT '公告',
  "api" longtext DEFAULT NULL,
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  UNIQUE KEY "security_uq_project_app_key" ("app_key"),
  KEY "project_manager_id" ("manager_id"),
  KEY "project_app_key" ("app_key"),
  KEY "project_create_time" ("create_time")
) ENGINE=InnoDB AUTO_INCREMENT=2 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "project"
--

LOCK TABLES "project" WRITE;
/*!40000 ALTER TABLE "project" DISABLE KEYS */;
INSERT INTO "project" VALUES
(1,1,'西瓜狗直播全家桶','iinXUS7O0IZW001ZAxPkIy3Bo9P21iLs','',1,0,1,1,1,0,1,1,1,'','','','',0,0,1,0,0,'','',0,0,'西瓜狗直播全家桶 · YY/狼音/音效 · 协议v2','','2026-07-20 05:41:31');
/*!40000 ALTER TABLE "project" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "project_build"
--

DROP TABLE IF EXISTS "project_build";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "project_build" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "manager_id" int(11) NOT NULL DEFAULT 0,
  "project_id" int(11) NOT NULL DEFAULT 0,
  "version" varchar(32) NOT NULL DEFAULT '',
  "build_hash" varchar(128) NOT NULL DEFAULT '',
  "note" varchar(120) NOT NULL DEFAULT '',
  "is_active" int(11) NOT NULL DEFAULT 1,
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  KEY "project_build_manager_id" ("manager_id"),
  KEY "project_build_project_id" ("project_id"),
  KEY "project_build_version" ("version"),
  KEY "project_build_build_hash" ("build_hash"),
  KEY "project_build_create_time" ("create_time"),
  KEY "project_build_project_id_build_hash_version_is_active" ("project_id","build_hash","version","is_active")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "project_build"
--

LOCK TABLES "project_build" WRITE;
/*!40000 ALTER TABLE "project_build" DISABLE KEYS */;
/*!40000 ALTER TABLE "project_build" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "project_login"
--

DROP TABLE IF EXISTS "project_login";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "project_login" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "manager_id" int(11) NOT NULL DEFAULT 0,
  "title" varchar(40) NOT NULL DEFAULT '',
  "mode" int(11) NOT NULL DEFAULT 0 COMMENT '0绑定登录,1普通登录,2点数登录',
  "reg_mode" int(11) NOT NULL DEFAULT 0 COMMENT '0带卡注册,1普通注册',
  "email_reg" int(11) NOT NULL DEFAULT 0 COMMENT '0关闭邮箱注册,1开启邮箱注册',
  "email_require" int(11) NOT NULL DEFAULT 0 COMMENT '0注册可不填邮箱,1注册必须邮箱',
  "email_verify" int(11) NOT NULL DEFAULT 0 COMMENT '0不强制验证,1注册需验证码,2登录前必须已验证',
  "unbind_mode" int(11) NOT NULL DEFAULT 0 COMMENT '0 不允许解绑 1 原机解绑 2 自动解绑 3 任意解绑',
  "unbind_weaken_mode" int(11) NOT NULL DEFAULT 0 COMMENT '0 不扣时 1 解绑就扣时 2 超出扣时 3 超出不扣时',
  "unbind_weaken" decimal(12,2) NOT NULL DEFAULT 0.00,
  "unbind_weaken_points" int(11) NOT NULL DEFAULT 1 COMMENT '解绑扣点',
  "unbind_times" int(11) NOT NULL DEFAULT 3 COMMENT '最大可解绑次数',
  "unbind_date" int(11) NOT NULL DEFAULT 3 COMMENT '0 天 1 月',
  "unbind_before" int(11) NOT NULL DEFAULT 0 COMMENT '解绑前置时间',
  "number_mode" int(11) NOT NULL DEFAULT 0 COMMENT '0 机器码控制 1 IP控制',
  "number_more" int(11) NOT NULL DEFAULT 10 COMMENT '最大限制',
  "number_weaken" int(11) NOT NULL DEFAULT 1 COMMENT '扣除点数',
  "number_weaken_time" int(11) NOT NULL DEFAULT 0 COMMENT '0 时 1天',
  "pc_more" int(11) NOT NULL DEFAULT 0 COMMENT '单机最大可在线',
  "pc_code_more" int(11) NOT NULL DEFAULT 0 COMMENT '最大多开上限',
  "heartbeat_min" int(11) NOT NULL DEFAULT 300 COMMENT '随机心跳最小间隔秒',
  "heartbeat_max" int(11) NOT NULL DEFAULT 900 COMMENT '随机心跳最大间隔秒',
  "heartbeat_tolerance" int(11) NOT NULL DEFAULT 300 COMMENT '心跳容错秒数',
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  KEY "project_login_manager_id" ("manager_id"),
  KEY "project_login_create_time" ("create_time")
) ENGINE=InnoDB AUTO_INCREMENT=2 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "project_login"
--

LOCK TABLES "project_login" WRITE;
/*!40000 ALTER TABLE "project_login" DISABLE KEYS */;
INSERT INTO "project_login" VALUES
(1,1,'anticrack-rule-9d0342-1784540491',0,0,1,1,1,0,0,0.00,0,0,0,0,0,1,0,0,0,0,60,300,60,'2026-07-20 05:41:31');
/*!40000 ALTER TABLE "project_login" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "project_notice"
--

DROP TABLE IF EXISTS "project_notice";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "project_notice" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "manager_id" int(11) NOT NULL DEFAULT 0,
  "project_id" int(11) NOT NULL DEFAULT 0 COMMENT '关联项目ID',
  "title" varchar(200) DEFAULT NULL COMMENT '公告标题',
  "content" longtext DEFAULT NULL COMMENT 'Markdown正文',
  "is_active" int(11) NOT NULL DEFAULT 1 COMMENT '0隐藏,1显示',
  "sort" int(11) NOT NULL DEFAULT 0 COMMENT '排序,越大越靠前',
  "create_time" datetime NOT NULL,
  "update_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  KEY "project_notice_manager_id" ("manager_id"),
  KEY "project_notice_project_id" ("project_id"),
  KEY "project_notice_is_active" ("is_active"),
  KEY "project_notice_sort" ("sort"),
  KEY "project_notice_create_time" ("create_time"),
  KEY "project_notice_update_time" ("update_time")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "project_notice"
--

LOCK TABLES "project_notice" WRITE;
/*!40000 ALTER TABLE "project_notice" DISABLE KEYS */;
/*!40000 ALTER TABLE "project_notice" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "project_var"
--

DROP TABLE IF EXISTS "project_var";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "project_var" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "manager_id" int(11) NOT NULL DEFAULT 0,
  "project_id" int(11) NOT NULL DEFAULT 0 COMMENT '关联项目ID',
  "label" varchar(200) DEFAULT NULL COMMENT '变量索引',
  "value" longtext DEFAULT NULL COMMENT '变量内容',
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  KEY "project_var_manager_id" ("manager_id"),
  KEY "project_var_project_id" ("project_id"),
  KEY "project_var_create_time" ("create_time")
) ENGINE=InnoDB AUTO_INCREMENT=44 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "project_var"
--

LOCK TABLES "project_var" WRITE;
/*!40000 ALTER TABLE "project_var" DISABLE KEYS */;
INSERT INTO "project_var" VALUES
(16,1,1,'product_code','yy_langyin_sfx','2026-07-20 05:45:26'),
(17,1,1,'product_name','西瓜狗直播全家桶','2026-07-20 05:45:26'),
(18,1,1,'product_targets','yy,langyin,sfx','2026-07-20 05:45:26'),
(19,1,1,'feature_yy_unlock','1','2026-07-20 05:45:26'),
(20,1,1,'feature_langyin','1','2026-07-20 05:45:26'),
(21,1,1,'feature_sfx','1','2026-07-20 05:45:26'),
(22,1,1,'feature_sfx_pack','default','2026-07-20 05:45:26'),
(23,1,1,'require_capability_path','1','2026-07-20 05:45:26'),
(24,1,1,'require_resource_alias','1','2026-07-20 05:45:26'),
(25,1,1,'decoy_mode','1','2026-07-20 05:45:26'),
(26,1,1,'material_epoch','1784540726','2026-07-20 05:45:26'),
(27,1,1,'semantic_rev','1','2026-07-20 05:45:26'),
(28,1,1,'chain_salt','fdc25187bc2c2e8cd2a8d2e8eca47a7f','2026-07-20 05:45:26'),
(29,1,1,'chain_schema','v1-yy','2026-07-20 05:45:26'),
(30,1,1,'gate_claim_salt','6ldXzeZxkW32ETPanMldz1ID','2026-07-20 05:45:26'),
(31,1,1,'min_client_version','1.0.0','2026-07-20 05:45:26'),
(32,1,1,'min_protocol_rev','2','2026-07-20 05:45:26'),
(33,1,1,'build_gate_mode','record','2026-07-20 05:45:26'),
(34,1,1,'allowed_channels','stable,beta','2026-07-20 05:45:26'),
(35,1,1,'service_enabled','1','2026-07-20 05:45:26'),
(36,1,1,'kill_switch','0','2026-07-20 05:45:26'),
(37,1,1,'yy_room_mode','standard','2026-07-20 05:45:26'),
(38,1,1,'langyin_profile','default','2026-07-20 05:45:26'),
(39,1,1,'sfx_quality','hq','2026-07-20 05:45:26'),
(40,1,1,'client_note','西瓜狗直播全家桶: 账号类型=QQ邮箱(account_mode=email); protocol v2 fk/cap required','2026-07-20 05:45:26'),
(41,1,1,'account_mode','email','2026-07-20 06:08:30'),
(42,1,1,'account_email_domain','qq.com','2026-07-20 06:08:30'),
(43,1,1,'register_identity','qq_email','2026-07-20 06:08:30');
/*!40000 ALTER TABLE "project_var" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "project_version"
--

DROP TABLE IF EXISTS "project_version";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "project_version" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "manager_id" int(11) NOT NULL DEFAULT 0,
  "project_id" int(11) NOT NULL DEFAULT 0 COMMENT '关联项目ID',
  "version" decimal(12,2) NOT NULL DEFAULT 1.00 COMMENT '版本号',
  "is_must_update" int(11) NOT NULL DEFAULT 0 COMMENT '0启用强制更新,1关闭强制更新',
  "is_active" int(11) NOT NULL DEFAULT 0 COMMENT '0启用,1关闭',
  "notice" longtext DEFAULT NULL COMMENT '更新公告',
  "wgt_url" varchar(255) DEFAULT NULL COMMENT '热更新文件下载地址',
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  KEY "project_version_manager_id" ("manager_id"),
  KEY "project_version_project_id" ("project_id"),
  KEY "project_version_is_must_update" ("is_must_update"),
  KEY "project_version_is_active" ("is_active"),
  KEY "project_version_create_time" ("create_time")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "project_version"
--

LOCK TABLES "project_version" WRITE;
/*!40000 ALTER TABLE "project_version" DISABLE KEYS */;
/*!40000 ALTER TABLE "project_version" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "protocol_v2_build_gate"
--

DROP TABLE IF EXISTS "protocol_v2_build_gate";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "protocol_v2_build_gate" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "manager_id" int(11) NOT NULL DEFAULT 0,
  "project_id" int(11) NOT NULL DEFAULT 0,
  "gate_type" varchar(24) NOT NULL DEFAULT '',
  "value" varchar(128) NOT NULL DEFAULT '',
  "action" varchar(16) NOT NULL DEFAULT 'deny',
  "note" varchar(120) DEFAULT NULL,
  "is_active" int(11) NOT NULL DEFAULT 1,
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  KEY "protocol_v2_build_gate_manager_id" ("manager_id"),
  KEY "protocol_v2_build_gate_project_id" ("project_id"),
  KEY "protocol_v2_build_gate_gate_type" ("gate_type"),
  KEY "protocol_v2_build_gate_value" ("value"),
  KEY "protocol_v2_build_gate_is_active" ("is_active"),
  KEY "protocol_v2_build_gate_create_time" ("create_time"),
  KEY "protocol_v2_build_gate_project_id_gate_type_is_active" ("project_id","gate_type","is_active"),
  KEY "protocol_v2_build_gate_project_id_value_is_active" ("project_id","value","is_active")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "protocol_v2_build_gate"
--

LOCK TABLES "protocol_v2_build_gate" WRITE;
/*!40000 ALTER TABLE "protocol_v2_build_gate" DISABLE KEYS */;
/*!40000 ALTER TABLE "protocol_v2_build_gate" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "protocol_v2_capability_lease"
--

DROP TABLE IF EXISTS "protocol_v2_capability_lease";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "protocol_v2_capability_lease" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "jti" varchar(64) NOT NULL DEFAULT '',
  "project_id" int(11) NOT NULL DEFAULT 0,
  "authorization_id" int(11) NOT NULL DEFAULT 0,
  "binding_id" varchar(64) NOT NULL DEFAULT '',
  "operation" varchar(64) NOT NULL DEFAULT '',
  "generation" bigint(20) NOT NULL DEFAULT 0,
  "lease_until" bigint(20) NOT NULL DEFAULT 0,
  "status" int(11) NOT NULL DEFAULT 1,
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  UNIQUE KEY "jti" ("jti"),
  KEY "protocol_v2_capability_lease_project_id" ("project_id"),
  KEY "protocol_v2_capability_lease_authorization_id" ("authorization_id"),
  KEY "protocol_v2_capability_lease_binding_id" ("binding_id"),
  KEY "protocol_v2_capability_lease_operation" ("operation"),
  KEY "protocol_v2_capability_lease_generation" ("generation"),
  KEY "protocol_v2_capability_lease_lease_until" ("lease_until"),
  KEY "protocol_v2_capability_lease_status" ("status"),
  KEY "protocol_v2_capability_lease_create_time" ("create_time"),
  KEY "protocol_v2_capability_lease_project_id_binding_id_status" ("project_id","binding_id","status"),
  KEY "protocol_v2_capability_lease_project_id_operation_status" ("project_id","operation","status"),
  KEY "protocol_v2_capability_lease_lease_until_status" ("lease_until","status")
) ENGINE=InnoDB AUTO_INCREMENT=41 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "protocol_v2_capability_lease"
--

LOCK TABLES "protocol_v2_capability_lease" WRITE;
/*!40000 ALTER TABLE "protocol_v2_capability_lease" DISABLE KEYS */;
INSERT INTO "protocol_v2_capability_lease" VALUES
(1,'h03l2eMmE1MWvvWUZ2nOJQ',1,1,'msMDus5-cU4WtU8akMTkmvLnaURFJtE9Bv2p2w_Ed5o','license.check',1,1784786313,1,'2026-07-23 01:56:15'),
(2,'BNa3xkaF5UCeknuux7Nu_A',1,1,'msMDus5-cU4WtU8akMTkmvLnaURFJtE9Bv2p2w_Ed5o','license.check',1,1784786410,1,'2026-07-23 01:56:15'),
(3,'UhE7VpJ24_TqaNwA2NqElQ',1,1,'msMDus5-cU4WtU8akMTkmvLnaURFJtE9Bv2p2w_Ed5o','license.check',1,1784786460,1,'2026-07-23 01:56:56'),
(4,'dPijRXgts94KGxDQ95Lh6w',1,1,'msMDus5-cU4WtU8akMTkmvLnaURFJtE9Bv2p2w_Ed5o','license.check',1,1784787323,1,'2026-07-23 02:11:11'),
(5,'kldYU0numdCKIgAfV0yOAQ',1,1,'msMDus5-cU4WtU8akMTkmvLnaURFJtE9Bv2p2w_Ed5o','license.check',1,1784787279,1,'2026-07-23 02:11:12'),
(6,'5bwZyXY3_vRJ1fAU450DSw',1,1,'msMDus5-cU4WtU8akMTkmvLnaURFJtE9Bv2p2w_Ed5o','license.check',1,1784787196,1,'2026-07-23 02:11:13'),
(7,'AbcWOZ3VRGcQlt7m4Oie1Q',1,1,'Nuqlc7FQBWUkeTsITJrJyTC9V3QJRl97kdx1HmfljCw','license.check',1,1784787386,1,'2026-07-23 02:11:15'),
(8,'A6QDcwYyexz4VCbJDPh3Rw',1,1,'T4gFLgY8NlKUg4nO0318QCJd8SEzue4oYJPDdxkmlEk','license.check',1,1784788068,1,'2026-07-23 02:23:48'),
(9,'hCi22nw39LqdQu436e3Hjw',1,1,'jdUEXizFbEoWFn8M5Zf-tvWWxOQtmlYcM3qOcsS0pRw','license.check',1,1784788021,1,'2026-07-23 02:24:28'),
(10,'zwX9NSNgXgDWmqKNfO9FMA',1,1,'RCO8BNYTs2_y9StvmpPtawZ9XLlA70ZBS5rC2aR5IA4','license.check',1,1784788978,1,'2026-07-23 02:39:57'),
(11,'QWPuvxvj3yAeG7xTW7syBQ',1,1,'RCO8BNYTs2_y9StvmpPtawZ9XLlA70ZBS5rC2aR5IA4','license.check',1,1784789373,1,'2026-07-23 02:43:45'),
(12,'bppLOMciX6BxzwlPUi0zcQ',1,1,'RCO8BNYTs2_y9StvmpPtawZ9XLlA70ZBS5rC2aR5IA4','license.check',1,1784791870,1,'2026-07-23 03:26:23'),
(13,'siGHLWcrEuJOuke_RO4EMw',1,1,'RCO8BNYTs2_y9StvmpPtawZ9XLlA70ZBS5rC2aR5IA4','license.check',1,1784791832,1,'2026-07-23 03:26:55'),
(14,'Myv7GDsPccWsUr8iX3Nn1w',1,1,'RCO8BNYTs2_y9StvmpPtawZ9XLlA70ZBS5rC2aR5IA4','license.check',1,1784791881,1,'2026-07-23 03:27:25'),
(15,'z4SincbQnRauDoUdOpxvqg',1,1,'RCO8BNYTs2_y9StvmpPtawZ9XLlA70ZBS5rC2aR5IA4','license.check',1,1784792018,1,'2026-07-23 03:28:01'),
(16,'pAwTnjsMUBznFDJSx1lZzQ',1,1,'fMMU-3vhExeedEp-9_CL6AHzJEYuec9U7_AViwOy5lA','license.check',1,1784792866,1,'2026-07-23 03:44:14'),
(17,'HeDfWYUUZx8n-fX7N8sjPg',1,1,'fMMU-3vhExeedEp-9_CL6AHzJEYuec9U7_AViwOy5lA','license.check',1,1784792995,1,'2026-07-23 03:44:41'),
(18,'hUlTGjkUdT0EVUdym4GP1A',1,1,'fMMU-3vhExeedEp-9_CL6AHzJEYuec9U7_AViwOy5lA','license.check',1,1784793076,1,'2026-07-23 03:45:47'),
(19,'1Nz0Xn55uabIvWxEJInAmQ',1,1,'fMMU-3vhExeedEp-9_CL6AHzJEYuec9U7_AViwOy5lA','license.check',1,1784792969,1,'2026-07-23 03:46:47'),
(20,'FItKAiGVgP7_UIoCLiENqw',1,1,'fMMU-3vhExeedEp-9_CL6AHzJEYuec9U7_AViwOy5lA','license.check',1,1784795360,1,'2026-07-23 04:23:46'),
(21,'2JtuzNJPsuQ8R5HbUlf5aQ',1,1,'fMMU-3vhExeedEp-9_CL6AHzJEYuec9U7_AViwOy5lA','license.check',1,1784796897,1,'2026-07-23 04:51:18'),
(22,'92eU2uh7Jp0e4dyg11JmUA',1,1,'RCO8BNYTs2_y9StvmpPtawZ9XLlA70ZBS5rC2aR5IA4','license.check',1,1784797874,1,'2026-07-23 05:08:47'),
(23,'OScKcHUqrVxeNvLjqo3G6A',1,1,'fMMU-3vhExeedEp-9_CL6AHzJEYuec9U7_AViwOy5lA','license.check',1,1784799052,1,'2026-07-23 05:25:42'),
(24,'DjgzTY4We9U6Tn385UI6Pg',1,1,'fMMU-3vhExeedEp-9_CL6AHzJEYuec9U7_AViwOy5lA','license.check',1,1784799571,1,'2026-07-23 05:35:24'),
(25,'iODcH7bl0rnZQPq4VmXSFA',1,1,'fMMU-3vhExeedEp-9_CL6AHzJEYuec9U7_AViwOy5lA','license.check',1,1784800149,1,'2026-07-23 05:43:15'),
(26,'QFixZmhCalxMNroNJ8U1Cg',1,1,'fMMU-3vhExeedEp-9_CL6AHzJEYuec9U7_AViwOy5lA','license.check',1,1784803732,1,'2026-07-23 06:46:24'),
(27,'NMinvTsSNG5nc4KNCN8FTQ',1,1,'fMMU-3vhExeedEp-9_CL6AHzJEYuec9U7_AViwOy5lA','license.check',1,1784804048,1,'2026-07-23 06:49:15'),
(28,'Tgv8kS5aQC33vBt_RNQlbg',1,1,'fMMU-3vhExeedEp-9_CL6AHzJEYuec9U7_AViwOy5lA','license.check',1,1784804264,1,'2026-07-23 06:52:34'),
(29,'7HXtW36nvSK33DgR8k99fg',1,1,'fMMU-3vhExeedEp-9_CL6AHzJEYuec9U7_AViwOy5lA','license.check',1,1784804597,1,'2026-07-23 06:59:48'),
(30,'mgtafzgoGy_Sb8WSts5GIQ',1,1,'EAT76jAkH6Z0NkknZo3hGQSFelZpbafdbPdTc_2a6kA','license.check',1,1784810274,1,'2026-07-23 08:32:34'),
(31,'0dZo4MSgMtEE7WhhwFq5FA',1,1,'EAT76jAkH6Z0NkknZo3hGQSFelZpbafdbPdTc_2a6kA','license.check',1,1784959815,1,'2026-07-25 02:06:58'),
(32,'Ifn-PIquGLu0YAmzi7gT2Q',1,1,'EAT76jAkH6Z0NkknZo3hGQSFelZpbafdbPdTc_2a6kA','license.check',1,1784962512,1,'2026-07-25 02:52:02'),
(33,'EIu_D7E6cxVmUq-2RbMXEw',1,1,'EAT76jAkH6Z0NkknZo3hGQSFelZpbafdbPdTc_2a6kA','license.check',1,1784968125,1,'2026-07-25 04:24:14'),
(34,'xLvzShCs3ukrx6Wn_Er3pg',1,1,'EAT76jAkH6Z0NkknZo3hGQSFelZpbafdbPdTc_2a6kA','license.check',1,1784969645,1,'2026-07-25 04:51:07'),
(35,'QSJTBBEVcKJKSRT7bJVEuQ',1,1,'EAT76jAkH6Z0NkknZo3hGQSFelZpbafdbPdTc_2a6kA','license.check',1,1784971931,1,'2026-07-25 05:26:30'),
(36,'-F2xNoqc-rTssa3-GvXP9Q',1,1,'EAT76jAkH6Z0NkknZo3hGQSFelZpbafdbPdTc_2a6kA','license.check',1,1784972519,1,'2026-07-25 05:37:11'),
(37,'-YGGswROcBKKaPzdSWQQnA',1,1,'EAT76jAkH6Z0NkknZo3hGQSFelZpbafdbPdTc_2a6kA','license.check',1,1785054837,1,'2026-07-26 04:31:41'),
(38,'NPvePB_aYE0_2b-kFZqlYg',1,1,'EAT76jAkH6Z0NkknZo3hGQSFelZpbafdbPdTc_2a6kA','license.check',1,1785059705,1,'2026-07-26 05:50:00'),
(39,'cxpZC6bcUKb09DqE0lYF3w',1,1,'EAT76jAkH6Z0NkknZo3hGQSFelZpbafdbPdTc_2a6kA','license.check',1,1785069633,1,'2026-07-26 08:36:40'),
(40,'JHIz706UxfDgOfPNOtPq2A',1,1,'EAT76jAkH6Z0NkknZo3hGQSFelZpbafdbPdTc_2a6kA','license.check',1,1788952142,1,'2026-09-09 07:05:29');
/*!40000 ALTER TABLE "protocol_v2_capability_lease" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "protocol_v2_challenge"
--

DROP TABLE IF EXISTS "protocol_v2_challenge";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "protocol_v2_challenge" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "session_id" varchar(64) NOT NULL DEFAULT '',
  "request_hash" varchar(64) NOT NULL DEFAULT '',
  "sequence_key" varchar(64) NOT NULL DEFAULT '',
  "logical_sequence_key" varchar(64) NOT NULL DEFAULT '',
  "project_id" int(11) NOT NULL DEFAULT 0,
  "project_ref" varchar(64) NOT NULL DEFAULT '',
  "operation" varchar(64) NOT NULL DEFAULT '',
  "operation_mode" varchar(32) NOT NULL DEFAULT '',
  "operation_points" int(11) NOT NULL DEFAULT 0,
  "operation_access" varchar(24) NOT NULL DEFAULT 'membership',
  "binding_id" varchar(64) NOT NULL DEFAULT '',
  "binding_generation" bigint(20) NOT NULL DEFAULT 1,
  "subject_hash" varchar(64) NOT NULL DEFAULT '',
  "authorization_id" int(11) NOT NULL DEFAULT 0,
  "device_public_key" varchar(64) NOT NULL DEFAULT '',
  "sequence" bigint(20) NOT NULL DEFAULT 0,
  "build_version" varchar(32) DEFAULT NULL,
  "build_hash" varchar(128) NOT NULL DEFAULT '',
  "scopes_json" longtext NOT NULL,
  "enrollment_token_hash" varchar(64) DEFAULT NULL,
  "enrollment_lock" varchar(64) NOT NULL DEFAULT '',
  "new_binding" int(11) NOT NULL DEFAULT 0,
  "session_key_box" longtext DEFAULT NULL,
  "plan_json" longtext NOT NULL,
  "transcript_hash" varchar(64) NOT NULL DEFAULT '',
  "expires_at" bigint(20) NOT NULL DEFAULT 0,
  "used_at" bigint(20) NOT NULL DEFAULT 0,
  "remote_ip" varchar(64) DEFAULT NULL,
  "state_mac" varchar(64) NOT NULL DEFAULT '',
  "verify_request_hash" varchar(64) DEFAULT NULL,
  "verify_response_json" longtext DEFAULT NULL,
  "verify_receipt_mac" varchar(64) DEFAULT NULL,
  "capability_jti" varchar(64) DEFAULT NULL,
  "capability_lease_until" bigint(20) NOT NULL DEFAULT 0,
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  UNIQUE KEY "session_id" ("session_id"),
  UNIQUE KEY "request_hash" ("request_hash"),
  UNIQUE KEY "sequence_key" ("sequence_key"),
  UNIQUE KEY "enrollment_lock" ("enrollment_lock"),
  KEY "protocol_v2_challenge_logical_sequence_key" ("logical_sequence_key"),
  KEY "protocol_v2_challenge_project_id" ("project_id"),
  KEY "protocol_v2_challenge_project_ref" ("project_ref"),
  KEY "protocol_v2_challenge_operation" ("operation"),
  KEY "protocol_v2_challenge_operation_mode" ("operation_mode"),
  KEY "protocol_v2_challenge_operation_access" ("operation_access"),
  KEY "protocol_v2_challenge_binding_id" ("binding_id"),
  KEY "protocol_v2_challenge_binding_generation" ("binding_generation"),
  KEY "protocol_v2_challenge_subject_hash" ("subject_hash"),
  KEY "protocol_v2_challenge_authorization_id" ("authorization_id"),
  KEY "protocol_v2_challenge_sequence" ("sequence"),
  KEY "protocol_v2_challenge_build_hash" ("build_hash"),
  KEY "protocol_v2_challenge_enrollment_token_hash" ("enrollment_token_hash"),
  KEY "protocol_v2_challenge_expires_at" ("expires_at"),
  KEY "protocol_v2_challenge_used_at" ("used_at"),
  KEY "protocol_v2_challenge_capability_lease_until" ("capability_lease_until"),
  KEY "protocol_v2_challenge_create_time" ("create_time")
) ENGINE=InnoDB AUTO_INCREMENT=53 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "protocol_v2_challenge"
--

LOCK TABLES "protocol_v2_challenge" WRITE;
/*!40000 ALTER TABLE "protocol_v2_challenge" DISABLE KEYS */;
/*!40000 ALTER TABLE "protocol_v2_challenge" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "protocol_v2_device_audit"
--

DROP TABLE IF EXISTS "protocol_v2_device_audit";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "protocol_v2_device_audit" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "project_id" int(11) NOT NULL DEFAULT 0,
  "binding_id" varchar(64) NOT NULL DEFAULT '',
  "authorization_id" int(11) NOT NULL DEFAULT 0,
  "actor_manager_id" int(11) NOT NULL DEFAULT 0,
  "action" varchar(16) NOT NULL DEFAULT '',
  "previous_status" int(11) NOT NULL DEFAULT 0,
  "new_status" int(11) NOT NULL DEFAULT 0,
  "previous_generation" bigint(20) NOT NULL DEFAULT 0,
  "new_generation" bigint(20) NOT NULL DEFAULT 0,
  "invalidated_challenges" bigint(20) NOT NULL DEFAULT 0,
  "reason" varchar(240) NOT NULL DEFAULT '',
  "remote_ip" varchar(64) DEFAULT NULL,
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  KEY "protocol_v2_device_audit_project_id" ("project_id"),
  KEY "protocol_v2_device_audit_binding_id" ("binding_id"),
  KEY "protocol_v2_device_audit_authorization_id" ("authorization_id"),
  KEY "protocol_v2_device_audit_actor_manager_id" ("actor_manager_id"),
  KEY "protocol_v2_device_audit_action" ("action"),
  KEY "protocol_v2_device_audit_previous_status" ("previous_status"),
  KEY "protocol_v2_device_audit_new_status" ("new_status"),
  KEY "protocol_v2_device_audit_previous_generation" ("previous_generation"),
  KEY "protocol_v2_device_audit_new_generation" ("new_generation"),
  KEY "protocol_v2_device_audit_create_time" ("create_time")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "protocol_v2_device_audit"
--

LOCK TABLES "protocol_v2_device_audit" WRITE;
/*!40000 ALTER TABLE "protocol_v2_device_audit" DISABLE KEYS */;
/*!40000 ALTER TABLE "protocol_v2_device_audit" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "protocol_v2_device_key"
--

DROP TABLE IF EXISTS "protocol_v2_device_key";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "protocol_v2_device_key" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "binding_id" varchar(64) NOT NULL DEFAULT '',
  "generation" bigint(20) NOT NULL DEFAULT 1,
  "project_id" int(11) NOT NULL DEFAULT 0,
  "project_ref" varchar(64) NOT NULL DEFAULT '',
  "subject_hash" varchar(64) NOT NULL DEFAULT '',
  "authorization_id" int(11) NOT NULL DEFAULT 0,
  "public_key" varchar(64) NOT NULL DEFAULT '',
  "scopes_json" longtext NOT NULL,
  "status" int(11) NOT NULL DEFAULT 1,
  "next_sequence" bigint(20) NOT NULL DEFAULT 1,
  "last_build_hash" varchar(128) DEFAULT NULL,
  "last_build_version" varchar(32) DEFAULT NULL,
  "first_ip" varchar(64) DEFAULT NULL,
  "last_ip" varchar(64) DEFAULT NULL,
  "first_seen_at" bigint(20) NOT NULL DEFAULT 0,
  "last_seen_at" bigint(20) NOT NULL DEFAULT 0,
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  UNIQUE KEY "binding_id" ("binding_id"),
  KEY "protocol_v2_device_key_generation" ("generation"),
  KEY "protocol_v2_device_key_project_id" ("project_id"),
  KEY "protocol_v2_device_key_project_ref" ("project_ref"),
  KEY "protocol_v2_device_key_subject_hash" ("subject_hash"),
  KEY "protocol_v2_device_key_authorization_id" ("authorization_id"),
  KEY "protocol_v2_device_key_status" ("status"),
  KEY "protocol_v2_device_key_next_sequence" ("next_sequence"),
  KEY "protocol_v2_device_key_first_seen_at" ("first_seen_at"),
  KEY "protocol_v2_device_key_last_seen_at" ("last_seen_at"),
  KEY "protocol_v2_device_key_create_time" ("create_time")
) ENGINE=InnoDB AUTO_INCREMENT=8 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "protocol_v2_device_key"
--

LOCK TABLES "protocol_v2_device_key" WRITE;
/*!40000 ALTER TABLE "protocol_v2_device_key" DISABLE KEYS */;
INSERT INTO "protocol_v2_device_key" VALUES
(1,'msMDus5-cU4WtU8akMTkmvLnaURFJtE9Bv2p2w_Ed5o',1,1,'4AVFzC_tMuotDqnnfTXvi0VqGoMCBDrUJyXWrLCLmC8','DVrsx1umQOwcj5Ao3OcnAV2Nq2-02YabkDE15UazeeU',1,'cyOGjRWLm-INbsqWqiNOVn3c4fZU7X7cxCvsa7TQlS8','[\"license.check\"]',1,7,'b036a80c194d75af974a6b3156ab1d7b809cdec85f0f6935958418149aa51c49','','240e:33d:79b0:e600:f081:4e61:e24f:188a','240e:33d:79b0:e600:f081:4e61:e24f:188a',1784786175,1784787073,'2026-07-23 01:56:15'),
(2,'Nuqlc7FQBWUkeTsITJrJyTC9V3QJRl97kdx1HmfljCw',1,1,'4AVFzC_tMuotDqnnfTXvi0VqGoMCBDrUJyXWrLCLmC8','DVrsx1umQOwcj5Ao3OcnAV2Nq2-02YabkDE15UazeeU',1,'evVPnOdUPTAZ-TvO9XYSA2PYTUNrbxh3MtnrUvtNHFo','[\"license.check\"]',1,2,'b036a80c194d75af974a6b3156ab1d7b809cdec85f0f6935958418149aa51c49','','240e:33d:79b0:e600:f081:4e61:e24f:188a','240e:33d:79b0:e600:f081:4e61:e24f:188a',1784787075,1784787075,'2026-07-23 02:11:15'),
(3,'T4gFLgY8NlKUg4nO0318QCJd8SEzue4oYJPDdxkmlEk',1,1,'4AVFzC_tMuotDqnnfTXvi0VqGoMCBDrUJyXWrLCLmC8','DVrsx1umQOwcj5Ao3OcnAV2Nq2-02YabkDE15UazeeU',1,'N763lDzhK3pmM8AbWlI9vGT9GQvwlDt7y4UeS2ZWh7U','[\"license.check\"]',1,2,'b036a80c194d75af974a6b3156ab1d7b809cdec85f0f6935958418149aa51c49','','240e:33d:79b0:e600:f081:4e61:e24f:188a','240e:33d:79b0:e600:f081:4e61:e24f:188a',1784787828,1784787828,'2026-07-23 02:23:48'),
(4,'jdUEXizFbEoWFn8M5Zf-tvWWxOQtmlYcM3qOcsS0pRw',1,1,'4AVFzC_tMuotDqnnfTXvi0VqGoMCBDrUJyXWrLCLmC8','DVrsx1umQOwcj5Ao3OcnAV2Nq2-02YabkDE15UazeeU',1,'QAP4KfjkPu0XvhlG--FXnnds-AHw341h_JOmqQOqnFg','[\"license.check\"]',1,2,'b036a80c194d75af974a6b3156ab1d7b809cdec85f0f6935958418149aa51c49','','240e:33d:79b0:e600:f081:4e61:e24f:188a','240e:33d:79b0:e600:f081:4e61:e24f:188a',1784787868,1784787868,'2026-07-23 02:24:28'),
(5,'RCO8BNYTs2_y9StvmpPtawZ9XLlA70ZBS5rC2aR5IA4',1,1,'4AVFzC_tMuotDqnnfTXvi0VqGoMCBDrUJyXWrLCLmC8','DVrsx1umQOwcj5Ao3OcnAV2Nq2-02YabkDE15UazeeU',1,'3XEupF_6QE1-xkwY_NmdFRsTFeJN2uXCAsEe2r-FNQQ','[\"license.check\"]',1,8,'b036a80c194d75af974a6b3156ab1d7b809cdec85f0f6935958418149aa51c49','','240e:33d:79b0:e600:f081:4e61:e24f:188a','64.188.8.149',1784788797,1784797727,'2026-07-23 02:39:57'),
(6,'fMMU-3vhExeedEp-9_CL6AHzJEYuec9U7_AViwOy5lA',1,1,'4AVFzC_tMuotDqnnfTXvi0VqGoMCBDrUJyXWrLCLmC8','DVrsx1umQOwcj5Ao3OcnAV2Nq2-02YabkDE15UazeeU',1,'v2xNOMCHBB_LtDnY12zGxD3dQknlfaK8Mf07mLe3z8s','[\"license.check\"]',1,14,'b036a80c194d75af974a6b3156ab1d7b809cdec85f0f6935958418149aa51c49','','240e:33d:79b0:e600:f081:4e61:e24f:188a','64.188.8.149',1784792654,1784804388,'2026-07-23 03:44:14'),
(7,'EAT76jAkH6Z0NkknZo3hGQSFelZpbafdbPdTc_2a6kA',1,1,'4AVFzC_tMuotDqnnfTXvi0VqGoMCBDrUJyXWrLCLmC8','DVrsx1umQOwcj5Ao3OcnAV2Nq2-02YabkDE15UazeeU',1,'J-lv_H8tnavCDtfeaBvdh5NetrPNI-YUqNwCbOcFy7A','[\"license.check\"]',1,12,'b036a80c194d75af974a6b3156ab1d7b809cdec85f0f6935958418149aa51c49','','64.188.8.149','106.42.178.204',1784809954,1788951929,'2026-07-23 08:32:34');
/*!40000 ALTER TABLE "protocol_v2_device_key" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "protocol_v2_exec_grant"
--

DROP TABLE IF EXISTS "protocol_v2_exec_grant";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "protocol_v2_exec_grant" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "exec_id" varchar(64) NOT NULL DEFAULT '',
  "project_id" int(11) NOT NULL DEFAULT 0,
  "authorization_id" int(11) NOT NULL DEFAULT 0,
  "binding_id" varchar(64) NOT NULL DEFAULT '',
  "operation" varchar(64) NOT NULL DEFAULT '',
  "resource" varchar(64) NOT NULL DEFAULT '',
  "cap_jti" varchar(64) NOT NULL DEFAULT '',
  "input_digest" varchar(64) NOT NULL DEFAULT '',
  "input_claim_key" varchar(64) NOT NULL DEFAULT '',
  "uses_left" int(11) NOT NULL DEFAULT 0,
  "exp" bigint(20) NOT NULL DEFAULT 0,
  "decoy" int(11) NOT NULL DEFAULT 0,
  "status" int(11) NOT NULL DEFAULT 0,
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  UNIQUE KEY "exec_id" ("exec_id"),
  UNIQUE KEY "input_claim_key" ("input_claim_key"),
  UNIQUE KEY "security_uq_protocol_v2_exec_input_claim" ("input_claim_key"),
  KEY "protocol_v2_exec_grant_project_id" ("project_id"),
  KEY "protocol_v2_exec_grant_authorization_id" ("authorization_id"),
  KEY "protocol_v2_exec_grant_binding_id" ("binding_id"),
  KEY "protocol_v2_exec_grant_operation" ("operation"),
  KEY "protocol_v2_exec_grant_resource" ("resource"),
  KEY "protocol_v2_exec_grant_cap_jti" ("cap_jti"),
  KEY "protocol_v2_exec_grant_exp" ("exp"),
  KEY "protocol_v2_exec_grant_status" ("status"),
  KEY "protocol_v2_exec_grant_create_time" ("create_time"),
  KEY "protocol_v2_exec_grant_project_id_binding_id_resource_exp" ("project_id","binding_id","resource","exp"),
  KEY "protocol_v2_exec_grant_project_id_cap_jti_resource" ("project_id","cap_jti","resource")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "protocol_v2_exec_grant"
--

LOCK TABLES "protocol_v2_exec_grant" WRITE;
/*!40000 ALTER TABLE "protocol_v2_exec_grant" DISABLE KEYS */;
/*!40000 ALTER TABLE "protocol_v2_exec_grant" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "protocol_v2_operation_policy"
--

DROP TABLE IF EXISTS "protocol_v2_operation_policy";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "protocol_v2_operation_policy" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "policy_key" varchar(96) NOT NULL DEFAULT '',
  "project_id" int(11) NOT NULL DEFAULT 0,
  "operation" varchar(64) NOT NULL DEFAULT '',
  "mode" varchar(32) NOT NULL DEFAULT '',
  "points" int(11) NOT NULL DEFAULT 0,
  "access_mode" varchar(24) NOT NULL DEFAULT 'membership',
  "status" int(11) NOT NULL DEFAULT 1,
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  UNIQUE KEY "policy_key" ("policy_key"),
  KEY "protocol_v2_operation_policy_project_id" ("project_id"),
  KEY "protocol_v2_operation_policy_operation" ("operation"),
  KEY "protocol_v2_operation_policy_mode" ("mode"),
  KEY "protocol_v2_operation_policy_access_mode" ("access_mode"),
  KEY "protocol_v2_operation_policy_status" ("status"),
  KEY "protocol_v2_operation_policy_create_time" ("create_time")
) ENGINE=InnoDB AUTO_INCREMENT=2 DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "protocol_v2_operation_policy"
--

LOCK TABLES "protocol_v2_operation_policy" WRITE;
/*!40000 ALTER TABLE "protocol_v2_operation_policy" DISABLE KEYS */;
INSERT INTO "protocol_v2_operation_policy" VALUES
(1,'1:license.check',1,'license.check','check',0,'membership',1,'2026-07-20 05:41:31');
/*!40000 ALTER TABLE "protocol_v2_operation_policy" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "protocol_v2_request_nonce"
--

DROP TABLE IF EXISTS "protocol_v2_request_nonce";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "protocol_v2_request_nonce" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "claim_key" varchar(64) NOT NULL DEFAULT '',
  "project_id" int(11) NOT NULL DEFAULT 0,
  "binding_id" varchar(128) NOT NULL DEFAULT '',
  "cap_jti" varchar(64) NOT NULL DEFAULT '',
  "action" varchar(16) NOT NULL DEFAULT '',
  "nonce" varchar(32) NOT NULL DEFAULT '',
  "expires_at" bigint(20) NOT NULL DEFAULT 0,
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  UNIQUE KEY "claim_key" ("claim_key"),
  KEY "protocol_v2_request_nonce_project_id" ("project_id"),
  KEY "protocol_v2_request_nonce_binding_id" ("binding_id"),
  KEY "protocol_v2_request_nonce_cap_jti" ("cap_jti"),
  KEY "protocol_v2_request_nonce_action" ("action"),
  KEY "protocol_v2_request_nonce_expires_at" ("expires_at"),
  KEY "protocol_v2_request_nonce_create_time" ("create_time"),
  KEY "protocol_v2_request_nonce_project_id_expires_at" ("project_id","expires_at"),
  KEY "protocol_v2_request_nonce_project_id_binding_id_action" ("project_id","binding_id","action")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "protocol_v2_request_nonce"
--

LOCK TABLES "protocol_v2_request_nonce" WRITE;
/*!40000 ALTER TABLE "protocol_v2_request_nonce" DISABLE KEYS */;
/*!40000 ALTER TABLE "protocol_v2_request_nonce" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "protocol_v2_resource"
--

DROP TABLE IF EXISTS "protocol_v2_resource";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "protocol_v2_resource" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "resource_key" varchar(160) NOT NULL DEFAULT '',
  "project_id" int(11) NOT NULL DEFAULT 0,
  "operation" varchar(64) NOT NULL DEFAULT '',
  "resource" varchar(64) NOT NULL DEFAULT '',
  "mode" varchar(24) NOT NULL DEFAULT 'seed',
  "ticket_ttl_sec" int(11) NOT NULL DEFAULT 0,
  "max_per_lease" int(11) NOT NULL DEFAULT 32,
  "module_ref" varchar(128) DEFAULT NULL,
  "status" int(11) NOT NULL DEFAULT 1,
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  UNIQUE KEY "resource_key" ("resource_key"),
  KEY "protocol_v2_resource_project_id" ("project_id"),
  KEY "protocol_v2_resource_operation" ("operation"),
  KEY "protocol_v2_resource_resource" ("resource"),
  KEY "protocol_v2_resource_status" ("status"),
  KEY "protocol_v2_resource_create_time" ("create_time")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "protocol_v2_resource"
--

LOCK TABLES "protocol_v2_resource" WRITE;
/*!40000 ALTER TABLE "protocol_v2_resource" DISABLE KEYS */;
/*!40000 ALTER TABLE "protocol_v2_resource" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "protocol_v2_resource_alias"
--

DROP TABLE IF EXISTS "protocol_v2_resource_alias";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "protocol_v2_resource_alias" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "project_id" int(11) NOT NULL DEFAULT 0,
  "binding_id" varchar(64) NOT NULL DEFAULT '',
  "cap_jti" varchar(64) NOT NULL DEFAULT '',
  "alias" varchar(32) NOT NULL DEFAULT '',
  "real_resource" varchar(64) NOT NULL DEFAULT '',
  "exp" bigint(20) NOT NULL DEFAULT 0,
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  KEY "protocol_v2_resource_alias_project_id" ("project_id"),
  KEY "protocol_v2_resource_alias_binding_id" ("binding_id"),
  KEY "protocol_v2_resource_alias_cap_jti" ("cap_jti"),
  KEY "protocol_v2_resource_alias_alias" ("alias"),
  KEY "protocol_v2_resource_alias_real_resource" ("real_resource"),
  KEY "protocol_v2_resource_alias_exp" ("exp"),
  KEY "protocol_v2_resource_alias_create_time" ("create_time"),
  KEY "protocol_v2_resource_alias_project_id_cap_jti_alias" ("project_id","cap_jti","alias"),
  KEY "protocol_v2_resource_alias_project_id_binding_id_exp" ("project_id","binding_id","exp")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "protocol_v2_resource_alias"
--

LOCK TABLES "protocol_v2_resource_alias" WRITE;
/*!40000 ALTER TABLE "protocol_v2_resource_alias" DISABLE KEYS */;
/*!40000 ALTER TABLE "protocol_v2_resource_alias" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "protocol_v2_risk_event"
--

DROP TABLE IF EXISTS "protocol_v2_risk_event";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "protocol_v2_risk_event" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "project_id" int(11) NOT NULL DEFAULT 0,
  "binding_id" varchar(64) DEFAULT NULL,
  "subject_key" varchar(64) DEFAULT NULL,
  "build_hash" varchar(128) DEFAULT NULL,
  "code" varchar(48) NOT NULL DEFAULT '',
  "score_delta" int(11) NOT NULL DEFAULT 0,
  "at" bigint(20) NOT NULL DEFAULT 0,
  "meta_hash" varchar(64) DEFAULT NULL,
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  KEY "protocol_v2_risk_event_project_id" ("project_id"),
  KEY "protocol_v2_risk_event_binding_id" ("binding_id"),
  KEY "protocol_v2_risk_event_subject_key" ("subject_key"),
  KEY "protocol_v2_risk_event_code" ("code"),
  KEY "protocol_v2_risk_event_at" ("at"),
  KEY "protocol_v2_risk_event_create_time" ("create_time"),
  KEY "protocol_v2_risk_event_project_id_binding_id_at" ("project_id","binding_id","at"),
  KEY "protocol_v2_risk_event_project_id_subject_key_at" ("project_id","subject_key","at")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "protocol_v2_risk_event"
--

LOCK TABLES "protocol_v2_risk_event" WRITE;
/*!40000 ALTER TABLE "protocol_v2_risk_event" DISABLE KEYS */;
/*!40000 ALTER TABLE "protocol_v2_risk_event" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "role"
--

DROP TABLE IF EXISTS "role";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "role" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "manager_id" int(11) NOT NULL DEFAULT 0 COMMENT '归属开发者',
  "level" int(11) NOT NULL DEFAULT 0 COMMENT '代理层级',
  "contact" int(11) NOT NULL DEFAULT 0 COMMENT '0等于代理层级可用，1大于代理层级可用',
  "title" varchar(20) DEFAULT NULL COMMENT '角色名称',
  "description" varchar(300) DEFAULT NULL COMMENT '角色功能描述',
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  KEY "role_manager_id" ("manager_id"),
  KEY "role_level" ("level"),
  KEY "role_contact" ("contact"),
  KEY "role_create_time" ("create_time")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "role"
--

LOCK TABLES "role" WRITE;
/*!40000 ALTER TABLE "role" DISABLE KEYS */;
/*!40000 ALTER TABLE "role" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "role_item"
--

DROP TABLE IF EXISTS "role_item";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "role_item" (
  "id" int(11) NOT NULL AUTO_INCREMENT,
  "role_id" int(11) NOT NULL DEFAULT 0 COMMENT '归属角色',
  "path" varchar(50) DEFAULT NULL COMMENT '请求路由',
  "index" varchar(50) DEFAULT NULL COMMENT '细粒化请求Key',
  "value" varchar(50) DEFAULT NULL COMMENT '细粒化请求value',
  "name" varchar(50) DEFAULT NULL COMMENT '权限名称',
  "create_time" datetime NOT NULL,
  PRIMARY KEY ("id"),
  KEY "role_item_role_id" ("role_id"),
  KEY "role_item_create_time" ("create_time")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "role_item"
--

LOCK TABLES "role_item" WRITE;
/*!40000 ALTER TABLE "role_item" DISABLE KEYS */;
/*!40000 ALTER TABLE "role_item" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Table structure for table "security_migration_marker"
--

DROP TABLE IF EXISTS "security_migration_marker";
/*!40101 SET @saved_cs_client     = @@character_set_client */;
/*!40101 SET character_set_client = utf8mb4 */;
CREATE TABLE "security_migration_marker" (
  "name" varchar(64) NOT NULL,
  "applied_at" bigint(20) NOT NULL DEFAULT 0,
  PRIMARY KEY ("name")
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_general_ci;
/*!40101 SET character_set_client = @saved_cs_client */;

--
-- Dumping data for table "security_migration_marker"
--

LOCK TABLES "security_migration_marker" WRITE;
/*!40000 ALTER TABLE "security_migration_marker" DISABLE KEYS */;
INSERT INTO "security_migration_marker" VALUES
('account_api_force_close_v1',1784539115);
/*!40000 ALTER TABLE "security_migration_marker" ENABLE KEYS */;
UNLOCK TABLES;

--
-- Dumping routines for database 'verification'
--
/*!40103 SET TIME_ZONE=@OLD_TIME_ZONE */;

/*!40101 SET SQL_MODE=@OLD_SQL_MODE */;
/*!40014 SET FOREIGN_KEY_CHECKS=@OLD_FOREIGN_KEY_CHECKS */;
/*!40014 SET UNIQUE_CHECKS=@OLD_UNIQUE_CHECKS */;
/*!40101 SET CHARACTER_SET_CLIENT=@OLD_CHARACTER_SET_CLIENT */;
/*!40101 SET CHARACTER_SET_RESULTS=@OLD_CHARACTER_SET_RESULTS */;
/*!40101 SET COLLATION_CONNECTION=@OLD_COLLATION_CONNECTION */;
/*!40111 SET SQL_NOTES=@OLD_SQL_NOTES */;

-- Dump completed on 2026-10-12  0:31:03
