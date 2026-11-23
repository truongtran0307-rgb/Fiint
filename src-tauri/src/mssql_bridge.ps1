$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Web.Extensions
Add-Type -ReferencedAssemblies System.Data, System.Web.Extensions, System.Xml -Language CSharp -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Data;
using System.Data.SqlClient;
using System.Globalization;
using System.IO;
using System.Runtime.InteropServices;
using System.Xml;
using System.Text;
using System.Web.Script.Serialization;

public static class VibeSqlBridge {
  static readonly JavaScriptSerializer Json = new JavaScriptSerializer { MaxJsonLength = int.MaxValue, RecursionLimit = 16 };

  static object Cell(object v) {
    if (v == null || v is DBNull) return null;
    if (v is string || v is bool || v is int || v is short || v is byte || v is double || v is float) return v;
    if (v is long) { long l = (long)v; return Math.Abs(l) < 9007199254740991L ? (object)l : l.ToString(CultureInfo.InvariantCulture); }
    if (v is decimal) return ((decimal)v).ToString(CultureInfo.InvariantCulture);
    if (v is DateTime) {
      DateTime d = (DateTime)v;
      return d.TimeOfDay == TimeSpan.Zero ? d.ToString("yyyy-MM-dd", CultureInfo.InvariantCulture) : d.ToString("yyyy-MM-dd HH:mm:ss.FFFFFFF", CultureInfo.InvariantCulture).TrimEnd('.');
    }
    if (v is DateTimeOffset) return ((DateTimeOffset)v).ToString("yyyy-MM-dd HH:mm:ss.FFFFFFF zzz", CultureInfo.InvariantCulture);
    if (v is TimeSpan) return ((TimeSpan)v).ToString();
    if (v is Guid) return v.ToString();
    if (v is byte[]) {
      byte[] b = (byte[])v;
      StringBuilder sb = new StringBuilder("0x");
      int n = Math.Min(b.Length, 64);
      for (int i = 0; i < n; i++) sb.Append(b[i].ToString("X2"));
      if (b.Length > n) sb.Append("... (" + b.Length + " bytes)");
      return sb.ToString();
    }
    return Convert.ToString(v, CultureInfo.InvariantCulture);
  }

  /* ---- SSMS's saved connections: UserSettings.xml (servers) + Windows Credential Manager (passwords) ---- */

  [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
  struct CREDENTIAL {
    public int Flags; public int Type; public string TargetName; public string Comment;
    public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
    public int CredentialBlobSize; public IntPtr CredentialBlob; public int Persist;
    public int AttributeCount; public IntPtr Attributes; public string TargetAlias; public string UserName;
  }
  [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
  static extern bool CredEnumerate(string filter, int flags, out int count, out IntPtr creds);
  [DllImport("advapi32.dll")]
  static extern void CredFree(IntPtr buffer);

  static string Key(string server, string user, string auth) { return (server + "|" + (auth == "0" ? "" : user) + "|" + auth).ToLowerInvariant(); }

  static Dictionary<string, object> Ssms() {
    // server|user|auth → entry; passwords from Credential Manager win over the XML (which keeps none)
    Dictionary<string, Dictionary<string, object>> found = new Dictionary<string, Dictionary<string, object>>();
    string root = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "Microsoft\\SQL Server Management Studio");
    if (Directory.Exists(root)) {
      foreach (string dir in Directory.GetDirectories(root)) {
        string file = Path.Combine(dir, "UserSettings.xml");
        if (!File.Exists(file)) continue;
        try {
          XmlDocument doc = new XmlDocument();
          doc.Load(file);
          foreach (XmlNode s in doc.SelectNodes("//ServerConnectionSettings")) {
            string server = Text(s, "Instance"), user = Text(s, "UserName"), auth = Text(s, "AuthenticationMethod");
            if (server == "") continue;
            Dictionary<string, object> e = new Dictionary<string, object>();
            e["server"] = server; e["user"] = user; e["auth"] = auth;
            string db = Text(s, "Database");
            e["database"] = db == "<default>" ? "" : db;
            foreach (XmlNode el in s.SelectNodes("Advanced/Element")) {
              string k = Text(el, "Key"), v = Text(el, "Value");
              if (k == "TSC") e["trustCert"] = v == "True";
              if (k == "EC") e["encrypt"] = v == "True";
            }
            e["source"] = "SSMS " + Path.GetFileName(dir);
            found[Key(server, user, auth)] = e;
          }
        } catch (Exception) { }
      }
    }
    int count; IntPtr list;
    if (CredEnumerate("Microsoft:SSMS:*", 0, out count, out list)) {
      try {
        for (int i = 0; i < count; i++) {
          CREDENTIAL c = (CREDENTIAL)Marshal.PtrToStructure(Marshal.ReadIntPtr(list, i * IntPtr.Size), typeof(CREDENTIAL));
          // Microsoft:SSMS:<version>:<server>:<user>:<server type guid>:<auth>
          string[] p = c.TargetName.Split(':');
          if (p.Length < 7) continue;
          string auth = p[p.Length - 1], user = p[p.Length - 3];
          string server = string.Join(":", p, 3, p.Length - 6);
          string pwd = c.CredentialBlobSize > 0 ? Marshal.PtrToStringUni(c.CredentialBlob, c.CredentialBlobSize / 2) : "";
          string k = Key(server, user, auth);
          Dictionary<string, object> e;
          if (!found.TryGetValue(k, out e)) {
            e = new Dictionary<string, object>();
            e["server"] = server; e["user"] = user; e["auth"] = auth; e["database"] = "";
            e["source"] = "SSMS " + p[2];
            found[k] = e;
          }
          e["password"] = pwd;
        }
      } finally { CredFree(list); }
    }
    Dictionary<string, object> res = new Dictionary<string, object>();
    res["ok"] = true;
    res["connections"] = new List<Dictionary<string, object>>(found.Values);
    return res;
  }

  static string Text(XmlNode n, string path) {
    XmlNode c = n.SelectSingleNode(path);
    return c == null ? "" : c.InnerText.Trim();
  }

  public static string Run(string line) {
    if (line.Contains("\"op\":\"ssms\"")) {
      Dictionary<string, object> r = Ssms();
      r["id"] = Json.Deserialize<Dictionary<string, object>>(line)["id"];
      return Json.Serialize(r);
    }
    Dictionary<string, object> res = new Dictionary<string, object>();
    List<object> results = new List<object>();
    List<string> messages = new List<string>();
    res["results"] = results;
    res["messages"] = messages;
    res["ok"] = true;
    DateTime started = DateTime.UtcNow;
    try {
      Dictionary<string, object> req = Json.Deserialize<Dictionary<string, object>>(line);
      res["id"] = req["id"];
      string connStr = (string)req["conn"];
      string sql = (string)req["sql"];
      int maxRows = req.ContainsKey("maxRows") ? Convert.ToInt32(req["maxRows"]) : 1000;
      int timeout = req.ContainsKey("timeout") ? Convert.ToInt32(req["timeout"]) : 60;
      bool readOnly = req.ContainsKey("readOnly") && Convert.ToBoolean(req["readOnly"]);
      int affected = -1;
      using (SqlConnection conn = new SqlConnection(connStr)) {
        conn.InfoMessage += delegate(object s, SqlInfoMessageEventArgs e) { messages.Add(e.Message); };
        conn.Open();
        SqlTransaction tx = readOnly ? conn.BeginTransaction() : null;
        try {
          using (SqlCommand cmd = new SqlCommand(sql, conn, tx)) {
            cmd.CommandTimeout = timeout;
            SqlDataReader reader = cmd.ExecuteReader();
            bool stopped = false;
            try {
              do {
                if (reader.FieldCount == 0) continue;
                List<object> cols = new List<object>();
                for (int i = 0; i < reader.FieldCount; i++) {
                  Dictionary<string, object> c = new Dictionary<string, object>();
                  c["name"] = reader.GetName(i);
                  c["type"] = reader.GetDataTypeName(i);
                  cols.Add(c);
                }
                List<object> rows = new List<object>();
                bool truncated = false;
                object[] vals = new object[reader.FieldCount];
                while (reader.Read()) {
                  if (rows.Count >= maxRows) { truncated = true; break; }
                  reader.GetValues(vals);
                  object[] row = new object[vals.Length];
                  for (int i = 0; i < vals.Length; i++) row[i] = Cell(vals[i]);
                  rows.Add(row);
                }
                Dictionary<string, object> r = new Dictionary<string, object>();
                r["columns"] = cols;
                r["rows"] = rows;
                r["truncated"] = truncated;
                results.Add(r);
                // more rows than we keep: stop the server instead of draining a huge result
                if (truncated) { cmd.Cancel(); stopped = true; break; }
              } while (reader.NextResult());
            } finally {
              try { reader.Close(); } catch (SqlException) { if (!stopped) throw; }
            }
            affected = reader.RecordsAffected;
          }
        } finally {
          if (tx != null) { try { tx.Rollback(); } catch (Exception) { } }
        }
      }
      res["affected"] = affected;
    } catch (SqlException e) {
      res["ok"] = false;
      List<string> parts = new List<string>();
      foreach (SqlError err in e.Errors) parts.Add((err.LineNumber > 0 ? "Line " + err.LineNumber + ": " : "") + err.Message);
      res["error"] = string.Join("\n", parts.ToArray());
    } catch (Exception e) {
      res["ok"] = false;
      res["error"] = e.Message;
    }
    res["ms"] = (int)(DateTime.UtcNow - started).TotalMilliseconds;
    return Json.Serialize(res);
  }
}
'@
$out = New-Object System.IO.StreamWriter([Console]::OpenStandardOutput(), (New-Object System.Text.UTF8Encoding $false))
$out.AutoFlush = $true
$out.WriteLine('{"ready":true}')
while ($true) {
  $line = [Console]::In.ReadLine()
  if ($line -eq $null) { break }
  if ($line.Trim().Length -eq 0) { continue }
  $out.WriteLine([VibeSqlBridge]::Run($line))
}
