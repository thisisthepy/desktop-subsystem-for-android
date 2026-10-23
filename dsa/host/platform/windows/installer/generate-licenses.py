#!/usr/bin/env python3
"""Generates the third-party licence bundle and master NOTICE for DSA.

It reads the dependency list from Cargo.lock and cargo metadata, validates that
every dependency complies with the licence policy in dsa/deny.toml, and writes
the licence bundle for all shipped components:
- Rust host binaries and crate dependencies
- Setup UI (.NET) and its dependencies
- WiX runtime pieces shipped in setup.exe
- Guest kernel (GPL-2.0)
- Gfxstream graphics transport
- AOSP guest image notices
"""

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

APACHE_2_0_TEXT = """                                 Apache License
                           Version 2.0, January 2004
                        http://www.apache.org/licenses/

   TERMS AND CONDITIONS FOR USE, REPRODUCTION, AND DISTRIBUTION

   1. Definitions.

      "License" shall mean the terms and conditions for use, reproduction,
      and distribution as defined by Sections 1 through 9 of this document.

      "Licensor" shall mean the copyright owner or entity authorized by
      the copyright owner that is granting the License.

      "Legal Entity" shall mean the union of the acting entity and all
      other entities that control, are controlled by, or are under common
      control with that entity. For the purposes of this definition,
      "control" means (i) the power, direct or indirect, to cause the
      direction or management of such entity, whether by contract or
      otherwise, or (ii) ownership of fifty percent (50%) or more of the
      outstanding shares, or (iii) beneficial ownership of such entity.

      "You" (or "Your") shall mean an individual or Legal Entity
      exercising permissions granted by this License.

      "Source" form shall mean the preferred form for making modifications,
      including but not limited to software source code, documentation
      source, and configuration files.

      "Object" form shall mean any form resulting from mechanical
      transformation or translation of a Source form, including but
      not limited to compiled object code, generated documentation,
      and conversions to other media types.

      "Work" shall mean the work of authorship, whether in Source or
      Object form, made available under the License, as indicated by a
      copyright notice that is included in or attached to the work.

      "Derivative Works" shall mean any work, whether in Source or Object
      form, that is based on (or derived from) the Work and for which the
      editorial revisions, annotations, elaborations, or other modifications
      represent, as a whole, an original work of authorship. For the purposes
      of this License, Derivative Works shall not include works that remain
      separable from, or merely link (or bind by name) to the interfaces of,
      the Work and Derivative Works thereof.

      "Contribution" shall mean any work of authorship, including
      the original version of the Work and any modifications or additions
      to that Work or Derivative Works thereof, that is intentionally
      submitted to Licensor for inclusion in the Work by the copyright owner
      or by an individual or Legal Entity authorized to submit on behalf of
      the copyright owner. For the purposes of this definition, "submitted"
      means any form of electronic, verbal, or written communication sent
      to the Licensor or its representatives, including but not limited to
      communication on electronic mailing lists, source code control systems,
      and issue tracking systems that are managed by, or on behalf of, the
      Licensor for the purpose of discussing and improving the Work, but
      excluding communication that is conspicuously marked or otherwise
      designated in writing by the copyright owner as "Not a Contribution."

      "Contributor" shall mean Licensor and any individual or Legal Entity
      on behalf of whom a Contribution has been received by Licensor and
      subsequently incorporated within the Work.

   2. Grant of Copyright License. Subject to the terms and conditions of
      this License, each Contributor hereby grants to You a perpetual,
      worldwide, non-exclusive, no-charge, royalty-free, irrevocable
      copyright license to reproduce, prepare Derivative Works of,
      publicly display, publicly perform, sublicense, and distribute the
      Work and such Derivative Works in Source or Object form.

   3. Grant of Patent License. Subject to the terms and conditions of
      this License, each Contributor hereby grants to You a perpetual,
      worldwide, non-exclusive, no-charge, royalty-free, irrevocable
      (except as stated in this section) patent license to make, have made,
      use, offer to sell, sell, import, and otherwise transfer the Work,
      where such license applies only to those patent claims licensable
      by such Contributor that are necessarily infringed by their
      Contribution(s) alone or by combination of their Contribution(s)
      with the Work to which such Contribution(s) was submitted. If You
      institute patent litigation against any entity (including a
      cross-claim or counterclaim in a lawsuit) alleging that the Work
      or a Contribution incorporated within the Work constitutes direct
      or contributory patent infringement, then any patent licenses
      granted to You under this License for that Work shall terminate
      as of the date such litigation is filed.

   4. Redistribution. You may reproduce and distribute copies of the
      Work or Derivative Works thereof in any medium, with or without
      modifications, and in Source or Object form, provided that You
      meet the following conditions:

      (a) You must give any other recipients of the Work or
          Derivative Works a copy of this License; and

      (b) You must cause any modified files to carry prominent notices
          stating that You changed the files; and

      (c) You must retain, in the Source form of any Derivative Works
          that You distribute, all copyright, patent, trademark, and
          attribution notices from the Source form of the Work,
          excluding those notices that do not pertain to any part of
          the Derivative Works; and

      (d) If the Work includes a "NOTICE" text file as part of its
          distribution, then any Derivative Works that You distribute must
          include a readable copy of the attribution notices contained
          within such NOTICE file, excluding those notices that do not
          pertain to any part of the Derivative Works, in at least one
          of the following places: within a NOTICE text file distributed
          as part of the Derivative Works; within the Source form or
          documentation, if provided along with the Derivative Works; or,
          within a display generated by the Derivative Works, if and
          wherever such third-party notices normally appear. The contents
          of the NOTICE file are for informational purposes only and
          do not modify the License. You may add Your own attribution
          notices within Derivative Works that You distribute, alongside
          or as an addendum to the NOTICE text from the Work, provided
          that such additional attribution notices cannot be construed
          as modifying the License.

      You may add Your own copyright statement to Your modifications and
      may provide additional or different license terms and conditions
      for use, reproduction, or distribution of Your modifications, or
      for any such Derivative Works as a whole, provided Your use,
      reproduction, and distribution of the Work otherwise complies with
      the conditions stated in this License.

   5. Submission of Contributions. Unless You explicitly state otherwise,
      any Contribution intentionally submitted for inclusion in the Work
      by You to the Licensor shall be under the terms and conditions of
      this License, without any additional terms or conditions.
      Notwithstanding the above, nothing herein shall supersede or modify
      the terms of any separate license agreement you may have executed
      with Licensor regarding such Contributions.

   6. Trademarks. This License does not grant permission to use the trade
      names, trademarks, service marks, or product names of the Licensor,
      except as required for reasonable and customary use in describing the
      origin of the Work and reproducing the content of the NOTICE file.

   7. Disclaimer of Warranty. Unless required by applicable law or
      agreed to in writing, Licensor provides the Work (and each
      Contributor provides its Contributions) on an "AS IS" BASIS,
      WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or
      implied, including, without limitation, any warranties or conditions
      of TITLE, NON-INFRINGEMENT, MERCHANTABILITY, or FITNESS FOR A
      PARTICULAR PURPOSE. You are solely responsible for determining the
      appropriateness of using or redistributing the Work and assume any
      risks associated with Your exercise of permissions under this License.

   8. Limitation of Liability. In no event and under no legal theory,
      whether in tort (including negligence), contract, or otherwise,
      unless required by applicable law (such as deliberate and grossly
      negligent acts) or agreed to in writing, shall any Contributor be
      liable to You for damages, including any direct, indirect, special,
      incidental, or consequential damages of any character arising as a
      result of this License or out of the use or inability to use the
      Work (including but not limited to damages for loss of goodwill,
      work stoppage, computer failure or malfunction, or any and all
      other commercial damages or losses), even if such Contributor
      has been advised of the possibility of such damages.

   9. Accepting Warranty or Additional Liability. While redistributing
      the Work or Derivative Works thereof, You may choose to offer,
      and charge a fee for, acceptance of support, warranty, indemnity,
      or other liability obligations and/or rights consistent with this
      License. However, in accepting such obligations, You may act only
      on Your own behalf and on Your sole responsibility, not on behalf
      of any other Contributor, and only if You agree to indemnify,
      defend, and hold each Contributor harmless for any liability
      incurred by, or claims asserted against, such Contributor by reason
      of your accepting any such warranty or additional liability.

   END OF TERMS AND CONDITIONS
"""

MIT_TEXT = """The MIT License (MIT)

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
"""

KERNEL_GPL_TEXT = """GNU GENERAL PUBLIC LICENSE
Version 2, June 1991

Copyright (C) 1989, 1991 Free Software Foundation, Inc.
51 Franklin Street, Fifth Floor, Boston, MA  02110-1301, USA

Everyone is permitted to copy and distribute verbatim copies
of this license document, but changing it is not allowed.

Preamble

The licenses for most software are designed to take away your
freedom to share and change it. By contrast, the GNU General Public
License is intended to guarantee your freedom to share and change free
software--to make sure the software is free for all its users. This
General Public License applies to most of the Free Software
Foundation's software and to any other program whose authors commit to
using it. (Some other Free Software Foundation software is covered by
the GNU Library General Public License instead.) You can apply it to
your programs, too.

When we speak of free software, we are referring to freedom, not
price. Our General Public Licenses are designed to make sure that you
have the freedom to distribute copies of free software (and charge for
this service if you wish), that you receive source code or can get it
if you want it, that you can change the software or use pieces of it
in new free programs; and that you know you can do these things.

To protect your rights, we need to make restrictions that forbid
anyone to deny you these rights or to ask you to surrender the rights.
These restrictions translate to certain responsibilities for you if you
distribute copies of the software, or if you modify it.

For example, if you distribute copies of such a program, whether
gratis or for a fee, you must give the recipients all the rights that
you have. You must make sure that they, too, receive or can get the
source code. And you must show them these terms so they know their
rights.

We protect your rights with two steps: (1) copyright the software, and
(2) offer you this license which gives you legal permission to copy,
distribute and/or modify the software.

Also, for each author's protection and ours, we want to make certain
that everyone understands that there is no warranty for this free
software. If the software is modified by someone else and passed on, we
want its recipients to know that what they have is not the original, so
that any problems introduced by others will not reflect on the original
authors' reputations.

Finally, any free program is threatened constantly by software
patents. We wish to avoid the danger that redistributors of a free
program will individually obtain patent licenses, in effect making the
program proprietary. To prevent this, we have made it clear that any
patent must be licensed for everyone's free use or not licensed at all.

The precise terms and conditions for copying, distribution and
modification follow.

TERMS AND CONDITIONS FOR COPYING, DISTRIBUTION AND MODIFICATION

0. This License applies to any program or other work which contains
a notice placed by the copyright holder saying it may be distributed
under the terms of this General Public License. The "Program", below,
refers to any such program or work, and a "work based on the Program"
means either the Program or any derivative work under copyright law:
that is to say, a work containing the Program or a portion of it,
either verbatim or with modifications and/or translated into another
language. (Hereinafter, translation is included without limitation in
the term "modification".) Each licensee is addressed as "you".

Activities other than copying, distribution and modification are not
covered by this License; they are outside its scope. The act of
running the Program is not restricted, and the output from the Program
is covered only if its contents constitute a work based on the
Program (independent of having been made by running the Program).
Whether that is true depends on what the Program does.

1. You may copy and distribute verbatim copies of the Program's
source code as you receive it, in any medium, provided that you
conspicuously and appropriately publish on each copy an appropriate
copyright notice and disclaimer of warranty; keep intact all the
notices that refer to this License and to the absence of any warranty;
and give any other recipients of the Program a copy of this License
along with the Program.

You may charge a fee for the physical act of transferring a copy, and
you may at your option offer warranty protection in exchange for a fee.

2. You may modify your copy or copies of the Program or any portion
of it, thus forming a work based on the Program, and copy and
distribute such modifications or work under the terms of Section 1
above, provided that you also meet all of these conditions:

a) You must cause the modified files to carry prominent notices
stating that you changed the files and the date of any change.

b) You must cause any work that you distribute or publish, that in
whole or in part contains or is derived from the Program or any
part thereof, to be licensed as a whole at no charge to all third
parties under the terms of this License.

c) If the modified program normally reads commands interactively
when run, you must cause it, when started running for such
interactive use in the most ordinary way, to print or display an
announcement including an appropriate copyright notice and a
notice that there is no warranty (or else, saying that you provide
a warranty) and that users may redistribute the program under
these conditions, and telling the user how to view a copy of this
License. (Exception: if the Program itself is interactive but
does not normally print such an announcement, your work based on
the Program is not required to print an announcement.)

These requirements apply to the modified work as a whole. If
identifiable sections of that work are not derived from the Program,
and can be reasonably considered independent and separate works in
themselves, then this License, and its terms, do not apply to those
sections when you distribute them as separate works. But when you
distribute the same sections as part of a whole which is a work based
on the Program, the distribution of the whole must be on the terms of
this License, whose permissions for other licensees extend to the
entire whole, and thus to each and every part regardless of who wrote it.

Thus, it is not the intent of this section to claim rights or contest
your rights to work written entirely by you; rather, the intent is to
exercise the right to control the distribution of derivative or
collective works based on the Program.

In addition, mere aggregation of another work not based on the Program
with the Program (or with a work based on the Program) on a volume of
a storage or distribution medium does not bring the other work under
the scope of this License.

3. You may copy and distribute the Program (or a work based on it,
under Section 2) in object code or executable form under the terms of
Sections 1 and 2 above provided that you also do one of the following:

a) Accompany it with the complete corresponding machine-readable
source code, which must be distributed under the terms of Sections
1 and 2 above on a medium customarily used for software interchange; or,

b) Accompany it with a written offer, valid for at least three
years, to give any third party, for a charge no more than your
cost of physically performing source distribution, a complete
machine-readable copy of the corresponding source code, to be
distributed under the terms of Sections 1 and 2 above on a medium
customarily used for software interchange; or,

c) Accompany it with the information you received as to the offer
to distribute corresponding source code. (This alternative is
allowed only for noncommercial distribution and only if you
received the program in object code or executable form with such
an offer, in accord with Subsection b above.)

The source code for a work means the preferred form of the work for
making modifications to it. For an executable work, complete source
code means all the source code for all modules it contains, plus any
associated interface definition files, plus the scripts used to
control compilation and installation of the executable. However, as a
special exception, the source code distributed need not include
anything that is normally distributed (in either source or binary
form) with the major components (compiler, kernel, and so on) of the
operating system on which the executable runs, unless that component
itself accompanies the executable.

If distribution of executable or object code is made by offering
access to copy from a designated place, then offering equivalent
access to copy the source code from the same place counts as
distribution of the source code, even though third parties are not
compelled to copy the source along with the object code.

4. You may not copy, modify, sublicense, or distribute the Program
except as expressly provided under this License. Any attempt
otherwise to copy, modify, sublicense or distribute the Program is
void, and will automatically terminate your rights under this License.
However, parties who have received copies, or rights, from you under
this License will not have their licenses terminated so long as such
parties remain in full compliance.

5. You are not required to accept this License, since you have not
signed it. However, nothing else grants you permission to modify or
distribute the Program or its derivative works. These actions are
prohibited by law if you do not accept this License. Therefore, by
modifying or distributing the Program (or any work based on the
Program), you indicate your acceptance of this License to do so, and
all its terms and conditions for copying, distributing or modifying
the Program or works based on it.

6. Each time you redistribute the Program (or any work based on the
Program), the recipient automatically receives a license from the
original licensor to copy, distribute or modify the Program subject to
these terms and conditions. You may not impose any further
restrictions on the recipients' exercise of the rights granted herein.
You are not responsible for enforcing compliance by third parties to
this License.

7. If, as a consequence of a court judgment or allegation of patent
infringement or for any other reason (not limited to patent issues),
conditions are imposed on you (whether by court order, agreement or
otherwise) that contradict the conditions of this License, they do not
excuse you from the conditions of this License. If you cannot
distribute so as to satisfy simultaneously your obligations under this
License and any other pertinent obligations, then as a consequence you
may not distribute the Program at all. For example, if a patent
license would not permit royalty-free redistribution of the Program by
all those who receive copies directly or indirectly through you, then
the only way you could satisfy both it and this License would be to
refrain entirely from distribution of the Program.

If any portion of this section is held invalid or unenforceable under
any particular circumstance, the balance of the section is intended to
apply and the section as a whole is intended to apply in other
circumstances.

It is not the purpose of this section to induce you to infringe any
patents or other property right claims or to contest validity of any
such claims; this section has the sole purpose of protecting the
integrity of the free software distribution system, which is
implemented by public license practices. Many people have made
generous contributions to the wide range of software distributed
through that system in reliance on consistent application of that
system; it is up to the author/donor to decide if he or she is willing
to distribute software through any other system and a licensee cannot
impose that choice.

This section is intended to make thoroughly clear what is believed to
be a consequence of the rest of this License.

8. If the distribution and/or use of the Program is restricted in
certain countries either by patents or by copyrighted interfaces, the
original copyright holder who puts the Program under this License
may add an explicit geographical distribution limitation excluding
those countries, so that distribution is permitted only in or among
countries not thus excluded. In such case, this License incorporates
the limitation as if written in the body of this License.

9. The Free Software Foundation may publish revised and/or new versions
of the General Public License from time to time. Such new versions will
be similar in spirit to the present version, but may differ in detail to
address new problems or concerns.

Each version is given a distinguishing version number. If the Program
specifies a version number of this License which applies to it and "any
later version", you have the option of following the terms and conditions
either of that version or of any later version published by the Free
Software Foundation. If the Program does not specify a version number of
this License, you may choose any version ever published by the Free Software
Foundation.

10. If you wish to incorporate parts of the Program into other free
programs whose distribution conditions are different, write to the author
to ask for permission. For software which is copyrighted by the Free
Software Foundation, write to the Free Software Foundation; we sometimes
make exceptions for this. Our decision will be guided by the two goals
of preserving the free status of all derivatives of our free software and
of promoting the sharing and reuse of software generally.

NO WARRANTY

11. BECAUSE THE PROGRAM IS LICENSED FREE OF CHARGE, THERE IS NO WARRANTY
FOR THE PROGRAM, TO THE EXTENT PERMITTED BY APPLICABLE LAW. EXCEPT WHEN
OTHERWISE STATED IN WRITING THE COPYRIGHT HOLDERS AND/OR OTHER PARTIES
PROVIDE THE PROGRAM "AS IS" WITHOUT WARRANTY OF ANY KIND, EITHER EXPRESSED
OR IMPLIED, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF
MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE. THE ENTIRE RISK AS
TO THE QUALITY AND PERFORMANCE OF THE PROGRAM IS WITH YOU. SHOULD THE
PROGRAM PROVE DEFECTIVE, YOU ASSUME THE COST OF ALL NECESSARY SERVICING,
REPAIR OR CORRECTION.

12. IN NO EVENT UNLESS REQUIRED BY APPLICABLE LAW OR AGREED TO IN WRITING
WILL ANY COPYRIGHT HOLDER, OR ANY OTHER PARTY WHO MAY MODIFY AND/OR
REDISTRIBUTE THE PROGRAM AS PERMITTED ABOVE, BE LIABLE TO YOU FOR DAMAGES,
INCLUDING ANY GENERAL, SPECIAL, INCIDENTAL OR CONSEQUENTIAL DAMAGES ARISING
OUT OF THE USE OR INABILITY TO USE THE PROGRAM (INCLUDING BUT NOT LIMITED
TO LOSS OF DATA OR DATA BEING RENDERED INACCURATE OR LOSSES SUSTAINED BY
YOU OR THIRD PARTIES OR A FAILURE OF THE PROGRAM TO OPERATE WITH ANY OTHER
PROGRAMS), EVEN IF SUCH HOLDER OR OTHER PARTY HAS BEEN ADVISED OF THE
POSSIBILITY OF SUCH DAMAGES.

END OF TERMS AND CONDITIONS
"""

WIX_MSRL_TEXT = """Microsoft Reciprocal License (MS-RL)

This license governs use of the accompanying software. If you use the software, you
accept this license. If you do not accept the license, do not use the software.

1. Definitions
The terms "reproduce," "reproduction," "derivative works," and "distribution" have the
same meaning here as under U.S. copyright law.
A "contribution" is the original software, or any additions or changes to the software.
A "contributor" is any person that distributes its contribution under this license.
"Licensed patents" are a contributor's patent claims that read directly on its contribution.

2. Grant of Rights
(A) Copyright Grant - Subject to the terms of this license, including the license conditions
and limitations in section 3, each contributor grants you a non-exclusive, worldwide,
royalty-free copyright license to reproduce its contribution, prepare derivative works of
its contribution, and distribute its contribution or any derivative works that you create.
(B) Patent Grant - Subject to the terms of this license, including the license conditions
and limitations in section 3, each contributor grants you a non-exclusive, worldwide,
royalty-free license under its licensed patents to make, have made, use, sell, offer for
sale, import, and/or otherwise dispose of its contribution in the software or derivative
works of the contribution in the software.

3. Conditions and Limitations
(A) Reciprocal Grants - For any file you distribute that contains code from the software
(in source code or binary format), you must provide recipients the source code to that
file along with a copy of this license, which license will apply to that file. The source
code must be either included in the distribution or freely accessible to anyone who
receives the software.
(B) No Trademark License - This license does not grant you rights to use any contributors'
name, logo, or trademarks.
(C) If you bring a patent claim against any contributor over patents that you claim are
infringed by the software, your patent license from such contributor to the software ends
automatically.
(D) If you distribute any portion of the software, you must retain all copyright, patent,
trademark, and attribution notices that are present in the software.
(E) If you distribute any portion of the software in source code form, you may do so only
under this license by including a complete copy of this license with your distribution. If
you distribute any portion of the software in compiled or object code form, you may only do
so under a license that complies with this license.
(F) Disclaimer of Warranty - The software is licensed "as-is." You bear the risk of using
it. The contributors give no express warranties, guarantees or conditions. You may have
additional consumer rights under your local laws which this license cannot change. To the
extent permitted under your local laws, the contributors exclude the implied warranties of
merchantability, fitness for a particular purpose and non-infringement.
"""

SETUP_UI_NOTICE = """Desktop Subsystem for Android Setup UI

The setup user interface (DsaSetup.exe) is developed as part of Desktop Subsystem for
Android and is licensed under the Apache License, Version 2.0.

It incorporates or links against the following third-party components:
- WixToolset.BootstrapperApplicationApi (5.0.2):
  Licensed under the Microsoft Reciprocal License (MS-RL).
  Source code available from the WiX Toolset project (https://github.com/wixtoolset).
- WixToolset Bootstrapper runtime (mbanative.dll):
  Licensed under the Microsoft Reciprocal License (MS-RL).
- Microsoft.NETFramework.ReferenceAssemblies (build-time targeting pack):
  Licensed under the MIT License.
"""

GFXSTREAM_NOTICE = """gfxstream Graphics Remoting Transport

Desktop Subsystem for Android includes patches and components from the Android Open Source
Project gfxstream repository (platform/hardware/google/gfxstream).

Copyright (C) Google LLC.
Licensed under the Apache License, Version 2.0 (the "License").
You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0

Portions of gfxstream and related guest dependencies (mesa3d, libhardware, system/core)
are licensed under the MIT License or BSD-3-Clause License.
"""

AOSP_NOTICE = """Android Open Source Project (AOSP) Guest Images

DSA guest system images are built from the Android Open Source Project (AOSP),
primarily licensed under the Apache License, Version 2.0.
Individual guest components may be subject to additional open-source licenses,
including the GNU General Public License v2.0 (for Linux kernel drivers and tools),
the BSD 2-Clause and 3-Clause licenses, and the MIT License.

Each published guest image carries its corresponding complete AOSP NOTICE file
(NOTICE.xml.gz or NOTICE.html.gz) alongside the image assets in GitHub Releases.
"""


def load_deny_allow_list(workspace_dir: Path) -> set:
    deny_path = workspace_dir / "deny.toml"
    if not deny_path.exists():
        deny_path = workspace_dir / "dsa" / "deny.toml"
    if not deny_path.exists():
        raise FileNotFoundError(f"deny.toml not found at {workspace_dir}")

    text = deny_path.read_text(encoding="utf-8")
    m = re.search(r"allow\s*=\s*\[(.*?)\]", text, re.DOTALL)
    if not m:
        raise ValueError(f"No allow list found in {deny_path}")
    raw_entries = m.group(1)
    allow = set(re.findall(r'"([^"]+)"', raw_entries))
    return allow


def is_allowed_expression(expr: str, allow_list: set) -> bool:
    if not expr:
        return False
    # Normalize slashes like MIT/Apache-2.0 to OR
    normalized = expr.replace("/", " OR ")
    tokens = re.split(r"(\(|\)|\bAND\b|\bOR\b)", normalized)
    py_expr = ""
    for t in tokens:
        stripped = t.strip()
        if not stripped:
            continue
        if stripped in ("AND", "OR", "(", ")"):
            py_expr += f" {stripped.lower()} "
        else:
            is_ok = stripped in allow_list
            py_expr += f" {is_ok} "
    try:
        return bool(eval(py_expr, {"__builtins__": {}}))
    except Exception as exc:
        raise ValueError(f"Failed to parse SPDX expression '{expr}': {exc}")


def get_packages_metadata(workspace_dir: Path):
    cmd = ["cargo", "metadata", "--format-version", "1"]
    res = subprocess.run(cmd, cwd=workspace_dir, capture_output=True, text=True, check=True)
    return json.loads(res.stdout)


def parse_cargo_lock(lock_path: Path):
    text = lock_path.read_text(encoding="utf-8")
    entries = re.findall(r'\[\[package\]\]\s+name = "([^"]+)"\s+version = "([^"]+)"', text)
    return entries


def generate_bundle(workspace_dir: Path, out_dir: Path, master_notice: Path | None = None):
    allow_list = load_deny_allow_list(workspace_dir)
    lock_path = workspace_dir / "Cargo.lock"
    if not lock_path.exists():
        lock_path = workspace_dir / "dsa" / "Cargo.lock"
    lock_packages = parse_cargo_lock(lock_path)

    metadata = get_packages_metadata(workspace_dir)
    pkg_by_key = {(p["name"], p["version"]): p for p in metadata["packages"]}

    # Validate all packages against licence policy
    violating = []
    crates_info = []
    for name, version in sorted(lock_packages):
        pkg = pkg_by_key.get((name, version))
        if not pkg:
            raise RuntimeError(f"Package {name} {version} in Cargo.lock not found in metadata")
        license_expr = pkg.get("license") or "UNKNOWN"
        if not is_allowed_expression(license_expr, allow_list):
            violating.append((name, version, license_expr))

        repo = pkg.get("repository") or ""
        authors = ", ".join(pkg.get("authors", []))
        crates_info.append({
            "name": name,
            "version": version,
            "license": license_expr,
            "repository": repo,
            "authors": authors,
        })

    if violating:
        msg = "\n".join(f"  {n} {v}: license '{lic}' is not in allow-list" for n, v, lic in violating)
        raise RuntimeError(f"Dependencies violate licence policy:\n{msg}")

    out_dir.mkdir(parents=True, exist_ok=True)

    # 1. rust-crates.txt
    rust_lines = [
        "Rust Host Dependencies",
        "======================",
        f"Total dependencies: {len(crates_info)}",
        "",
    ]
    for c in crates_info:
        info_line = f"{c['name']} {c['version']}: {c['license']}"
        if c["repository"]:
            info_line += f" ({c['repository']})"
        elif c["authors"]:
            info_line += f" ({c['authors']})"
        rust_lines.append(info_line)
    rust_crates_text = "\n".join(rust_lines) + "\n"
    (out_dir / "rust-crates.txt").write_text(rust_crates_text, encoding="utf-8")

    # 2. Component licenses
    (out_dir / "kernel-GPL-2.0.txt").write_text(KERNEL_GPL_TEXT, encoding="utf-8")
    (out_dir / "wix-MS-RL.txt").write_text(WIX_MSRL_TEXT, encoding="utf-8")
    (out_dir / "setup-ui.txt").write_text(SETUP_UI_NOTICE, encoding="utf-8")
    (out_dir / "gfxstream.txt").write_text(GFXSTREAM_NOTICE, encoding="utf-8")
    (out_dir / "aosp-notice.txt").write_text(AOSP_NOTICE, encoding="utf-8")
    (out_dir / "apache-2.0.txt").write_text(APACHE_2_0_TEXT, encoding="utf-8")
    (out_dir / "mit.txt").write_text(MIT_TEXT, encoding="utf-8")

    # 3. Master NOTICE.txt
    master_lines = [
        "================================================================================",
        "Desktop Subsystem for Android (DSA)",
        "Copyright thisisthepy. All rights reserved.",
        "================================================================================",
        "",
        "Licensed under the Apache License, Version 2.0 (the \"License\");",
        "you may not use this file except in compliance with the License.",
        "You may obtain a copy of the License at",
        "",
        "    http://www.apache.org/licenses/LICENSE-2.0",
        "",
        "Unless required by applicable law or agreed to in writing, software",
        "distributed under the License is distributed on an \"AS IS\" BASIS,",
        "WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.",
        "See the License for the specific language governing permissions and",
        "limitations under the License.",
        "",
        "--------------------------------------------------------------------------------",
        "THIRD-PARTY COMPONENTS AND LICENSES",
        "--------------------------------------------------------------------------------",
        "",
        "This package includes or distributes the following third-party components:",
        "",
        "1. Guest Kernel (bzImage)",
        "   - Source: Microsoft WSL2 Linux Kernel with DSA binder and partition patches",
        "   - License: GNU General Public License v2.0 (GPL-2.0-only)",
        "   - Full license text: see licenses/kernel-GPL-2.0.txt",
        "   - Kernel configurations and build scripts: dsa/kernel/ in the source repository",
        "",
        "2. Windows Setup Program and WiX Runtime (setup.exe, DsaSetup.exe)",
        "   - WixToolset.BootstrapperApplicationApi, mbanative.dll, WiX 5 Burn engine",
        "   - License: Microsoft Reciprocal License (MS-RL)",
        "   - Full license text: see licenses/wix-MS-RL.txt and licenses/setup-ui.txt",
        "",
        "3. Graphics Remoting Path (gfxstream)",
        "   - Source: Android Open Source Project gfxstream",
        "   - License: Apache License 2.0 with BSD-3-Clause and MIT subcomponents",
        "   - Full details: see licenses/gfxstream.txt",
        "",
        "4. Android Guest System Images",
        "   - Source: Android Open Source Project (AOSP)",
        "   - License: Apache License 2.0, GNU General Public License v2.0, BSD, MIT",
        "   - Detailed attribution notices: carried alongside each image release asset",
        "",
        "5. Rust Host Binaries and Crate Dependencies",
        "   - Full list of all Rust dependencies, versions and licenses:",
        "",
    ]
    for c in crates_info:
        item = f"   * {c['name']} {c['version']}: {c['license']}"
        if c["repository"]:
            item += f" ({c['repository']})"
        master_lines.append(item)

    master_lines.extend([
        "",
        "For additional license texts, refer to the files in the licenses/ directory.",
        "",
    ])
    master_text = "\n".join(master_lines) + "\n"
    (out_dir / "NOTICE.txt").write_text(master_text, encoding="utf-8")

    if master_notice:
        master_notice.parent.mkdir(parents=True, exist_ok=True)
        master_notice.write_text(master_text, encoding="utf-8")

    print(f"Generated licence bundle in {out_dir} with {len(crates_info)} Rust crates.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workspace", type=Path, default=Path(__file__).resolve().parents[4] / "dsa")
    parser.add_argument("--out-dir", type=Path, default=Path(__file__).resolve().parents[4] / "dsa/target/installer/stage/licenses")
    parser.add_argument("--master-notice", type=Path, default=None)
    parser.add_argument("--check-only", action="store_true")
    args = parser.parse_args()

    allow_list = load_deny_allow_list(args.workspace)
    metadata = get_packages_metadata(args.workspace)
    lock_path = args.workspace / "Cargo.lock"
    lock_packages = parse_cargo_lock(lock_path)
    pkg_by_key = {(p["name"], p["version"]): p for p in metadata["packages"]}

    violating = []
    for name, version in lock_packages:
        pkg = pkg_by_key.get((name, version))
        if not pkg:
            sys.exit(f"Package {name} {version} in Cargo.lock not found in metadata")
        license_expr = pkg.get("license") or "UNKNOWN"
        if not is_allowed_expression(license_expr, allow_list):
            violating.append((name, version, license_expr))

    if violating:
        for n, v, lic in violating:
            print(f"Error: {n} {v} has license '{lic}' which is not allowed by policy", file=sys.stderr)
        sys.exit(1)

    if args.check_only:
        print(f"All {len(lock_packages)} Rust dependencies comply with the licence policy.")
        return

    generate_bundle(args.workspace, args.out_dir, args.master_notice)


if __name__ == "__main__":
    main()
