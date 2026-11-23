"""Generate the per-language tables ytnative reads YouTube's wording with, and the interface's
date formats, from CLDR (Babel) plus what YouTube itself was seen to answer.

    python -m pip install --target <dir> babel==2.16.0
    PYTHONPATH=<dir> python tools/gen-lang-data.py

Writes crates/yt-core/src/lang_data.rs and crates/app/src/i18n_dates.rs (both checked in; run
again only to change them). YouTube's "N days ago" wording is CLDR's relative time; the words
CLDR has no field for (live viewers, past streams, the Watch later / Liked playlists) are copied
from YouTube's answers in each language (ytcli with YTCLI_HL, November 2026).
"""
import os
import re

from babel import Locale

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# YouTube hl -> CLDR locale. The interface's own codes differ for three (he = iw, nb = no,
# zh = zh-CN); `i18n::youtube_hl` maps them.
LANGS = {
    'en': 'en', 'hu': 'hu', 'de': 'de', 'es': 'es', 'fr': 'fr', 'ru': 'ru', 'it': 'it',
    'pl': 'pl', 'pt': 'pt', 'nl': 'nl', 'tr': 'tr', 'uk': 'uk', 'cs': 'cs', 'sk': 'sk', 'sl': 'sl',
    'hr': 'hr', 'sr': 'sr', 'ro': 'ro', 'bg': 'bg', 'el': 'el', 'lt': 'lt', 'lv': 'lv', 'et': 'et',
    'fi': 'fi', 'sv': 'sv', 'no': 'nb', 'da': 'da', 'ca': 'ca', 'gl': 'gl', 'sq': 'sq', 'az': 'az',
    'hy': 'hy', 'ka': 'ka', 'ar': 'ar', 'iw': 'he', 'fa': 'fa', 'hi': 'hi', 'te': 'te', 'th': 'th',
    'vi': 'vi', 'id': 'id', 'ms': 'ms', 'ja': 'ja', 'ko': 'ko', 'zh-CN': 'zh_Hans_CN',
    'zh-TW': 'zh_Hant_TW',
}

# A live stream's viewer line ("9898 人が視聴中"), lower case.
WATCHING = {
    'en': ['watching'], 'hu': ['néz'], 'de': ['zuschauer'], 'es': ['espectador'],
    'fr': ['spectateur'], 'ru': ['смотр', 'зрител'], 'it': ['spettator'],
    'pl': ['oglądających'], 'pt': ['assistindo'], 'nl': ['kijker'], 'tr': ['izliyor'],
    'uk': ['дивляться'], 'cs': ['sledující'], 'sk': ['diváci'], 'sl': ['gledalcev'],
    'hr': ['gledatelja'], 'sr': ['гледалаца'], 'ro': ['vizionează'], 'bg': ['гледат'],
    'el': ['παρακολουθούν'], 'lt': ['žiūri'], 'lv': ['skatās'], 'et': ['vaatab'],
    'fi': ['katsojaa'], 'sv': ['tittare'], 'no': ['ser på'], 'da': ['ser med'],
    'ca': ['espectadors'], 'gl': ['espectadores'], 'sq': ['po shikojnë'], 'az': ['izləmə'],
    'hy': [], 'ka': ['უყურებს'], 'ar': ['حاليًا'], 'iw': ['צופים'], 'fa': ['در حال تماشا'],
    'hi': ['दर्शकों की संख्या'], 'te': ['చూస్తున్నారు'], 'th': ['ดูอยู่'], 'vi': ['đang xem'],
    'id': ['sedang menonton'], 'ms': ['menonton'], 'ja': ['視聴中'], 'ko': ['시청 중'],
    'zh-CN': ['正在观看'], 'zh-TW': ['正在觀看'],
}

# The words that turn an age into a past live stream's ("Streamed 2 days ago"), lower case.
STREAMED = {
    'en': ['streamed'], 'hu': ['streamelve'], 'de': ['gestreamt'], 'es': ['emitido'],
    'fr': ['diffusé'], 'ru': ['трансляция'], 'it': ['trasmesso'],
    'pl': ['transmisja odbyła się'], 'pt': ['transmitido'], 'nl': ['gestreamd'],
    'tr': ['önce yayınlandı'], 'uk': ['трансляція відбулася'], 'cs': ['vysíláno'],
    'sk': ['streamované'], 'sl': ['pretočno predvajano'], 'hr': ['vrijeme streaminga'],
    'sr': ['стримовано'], 'ro': ['transmis live'], 'bg': ['предавано поточно'],
    'el': ['μεταδόθηκε'], 'lt': ['perduota srautu'], 'lv': ['straumēts'],
    'et': ['otseülekanne toimus'], 'fi': ['striimattu'], 'sv': ['streamat'], 'no': ['strømmet'],
    'da': ['blev streamet'], 'ca': ['emès'], 'gl': ['emitiuse'], 'sq': ['transmetuar'],
    'az': ['yayım vaxtı'], 'hy': ['հեռարձակումն ավարտվել է'], 'ka': ['სტრიმინგი შესრულდა'],
    'ar': ['تم البث'], 'iw': ['שודר'], 'fa': ['پخش جریانی'], 'hi': ['स्ट्रीम किया गया'],
    'te': ['స్ట్రీమ్ అయ్యింది'], 'th': ['สตรีมแล้ว'], 'vi': ['phát trực tiếp'],
    'id': ['streaming'], 'ms': ['distrim'], 'ja': ['配信済み'], 'ko': ['스트리밍 시간'],
    'zh-CN': ['直播时间'], 'zh-TW': ['直播時間'],
}

# YouTube's titles of the Watch later and Liked videos playlists.
PLAYLISTS = {
    'en': ('Watch later', 'Liked videos'), 'hu': ('Megnézendő videók', 'Kedvelt videók'),
    'de': ('Später ansehen', 'Videos, die ich mag'), 'es': ('Ver más tarde', 'Vídeos que me gustan'),
    'fr': ('À regarder plus tard', "Vidéos J'aime"), 'ru': ('Смотреть позже', 'Понравившиеся'),
    'it': ('Guarda più tardi', 'Video piaciuti'), 'pl': ('Do obejrzenia', 'Polubione filmy'),
    'pt': ('Assistir mais tarde', 'Vídeos com "Gostei"'), 'nl': ('Later bekijken', "Gelikete video's"),
    'tr': ('Daha sonra izle', 'Beğendiğim videolar'), 'uk': ('Переглянути пізніше', 'Відео, які сподобалися'),
    'cs': ('Přehrát později', 'Oblíbená videa'), 'sk': ('Pozrieť neskôr', 'Obľúbené videá'),
    'sl': ('Poznejši ogled', 'Všečkani videoposnetki'), 'hr': ('Gledat ću kasnije', 'Omiljeni videozapisi'),
    'sr': ('За касније', 'Лајковани видеи'), 'ro': ('Vizionează mai târziu', 'Clipuri apreciate'),
    'bg': ('За гледане по-късно', 'Харесани видеоклипове'),
    'el': ('Παρακολούθηση αργότερα', 'Βίντεο που σας αρέσουν'),
    'lt': ('Žiūrėti vėliau', 'Patikę vaizdo įrašai'), 'lv': ('Skatīties vēlāk', 'Patīk'),
    'et': ('Hiljem vaatamiseks', 'Meeldinud videod'), 'fi': ('Katso myöhemmin', 'Tykätyt videot'),
    'sv': ('Titta senare', 'Videor du gillat'), 'no': ('Se senere', 'Likte videoer'),
    'da': ('Se senere', 'Videoer, du kan lide'), 'ca': ('Visualitza més tard', "Vídeos que m'agraden"),
    'gl': ('Ver máis tarde', 'Vídeos que me gustan'), 'sq': ('Shiko më vonë', 'Video të pëlqyera'),
    'az': ('Sonra izləyin', 'Bəyənilən videolar'), 'hy': ('Դիտել ավելի ուշ', 'Հավանած տեսանյութեր'),
    'ka': ('მოგვიანებით სანახავი', 'მოწონებული ვიდეოები'), 'ar': ('المشاهدة لاحقًا', 'فيديوهات أعجبتك'),
    'iw': ('לצפייה בהמשך', 'סרטונים שאהבתי'), 'fa': ('تماشا در فرصتی دیگر', 'ویدیوهای پسندیده شده'),
    'hi': ('बाद में देखें', 'पसंद किए गए वीडियो'), 'te': ('తర్వాత చూడండి', 'లైక్ చేసిన వీడియోలు'),
    'th': ('ดูภายหลัง', 'วิดีโอที่ชอบ'), 'vi': ('Xem sau', 'Video đã thích'),
    'id': ('Tonton nanti', 'Video yang disukai'), 'ms': ('Tonton kemudian', 'Video yang disukai'),
    'ja': ('後で見る', '高く評価した動画'), 'ko': ('나중에 볼 동영상', '좋아요 표시한 동영상'),
    'zh-CN': ('稍后观看', '赞过的视频'), 'zh-TW': ('稍後觀看', '喜歡的影片'),
}

# YouTube's wording where it is not CLDR's: Azerbaijani says "əvvəl" where CLDR says "öncə".
AGO_SWAP = {'az': [('öncə', 'əvvəl')]}

UNITS = [('second', 1), ('minute', 60), ('hour', 3600), ('day', 86400), ('week', 7 * 86400),
         ('month', 30 * 86400), ('year', 365 * 86400)]
FIXED_N = {'zero': 1, 'one': 1, 'two': 2}


def norm(s):
    """What `lang_data::norm` does to YouTube's text: lower case, one kind of space, no bidi marks."""
    s = re.sub('[‎‏‪-‮⁦-⁩]', '', s)
    return re.sub(r'[\s   ]+', ' ', s).strip().lower()


def ago_forms(hl):
    loc = Locale.parse(LANGS[hl])
    out = set()
    for unit, secs in UNITS:
        for width in ('', '-short', '-narrow'):
            f = loc._data['date_fields'].get(unit + width)
            if not f or 'past' not in f:
                continue
            for cat, pat in f['past'].items():
                pats = [norm(pat)]
                for a, b in AGO_SWAP.get(hl, []):
                    pats.append(pats[0].replace(a, b))
                for p in pats:
                    if '{0}' in p:
                        before, after = p.split('{0}')
                        if before.strip() or after.strip():
                            out.add((before, after, 0, secs))
                    elif cat in FIXED_N:
                        out.add((p, '', FIXED_N[cat], secs))
    return sorted(out)


def month_names(hl):
    loc = Locale.parse(LANGS[hl])
    names = [set() for _ in range(12)]
    for ctx in ('format', 'stand-alone'):
        for width in ('wide', 'abbreviated'):
            for i in range(1, 13):
                n = re.sub(r"^(de |d’|d')", '', norm(loc.months[ctx][width][i]).rstrip('.'))
                if n and not re.search(r'\d', n):
                    names[i - 1].add(n)
    return [sorted(s, key=lambda x: (-len(x), x)) for s in names]


def rs(s):
    return '"' + s.replace('\\', '\\\\').replace('"', '\\"') + '"'


def rs_list(xs):
    return '&[' + ', '.join(rs(x) for x in xs) + ']'


def gen_core():
    out = ['//! Generated by `tools/gen-lang-data.py`: do not edit by hand.',
           '//! YouTube\'s wording per content language (`hl`), from CLDR and YouTube\'s answers.',
           '', 'use super::{Ago, Lang};', '',
           'pub(super) static LANGS: &[Lang] = &[']
    for hl in LANGS:
        later, liked = PLAYLISTS[hl]
        out.append('    Lang {')
        out.append(f'        hl: {rs(hl)},')
        out.append('        months: [' + ', '.join(rs_list(m) for m in month_names(hl)) + '],')
        out.append('        ago: &[')
        for before, after, n, secs in ago_forms(hl):
            out.append(f'            Ago {{ before: {rs(before)}, after: {rs(after)}, n: {n}, secs: {secs} }},')
        out.append('        ],')
        out.append(f'        watching: {rs_list(WATCHING[hl])},')
        out.append(f'        streamed: {rs_list(STREAMED[hl])},')
        out.append(f'        later: {rs(norm(later))},')
        out.append(f'        liked: {rs(norm(liked))},')
        out.append('    },')
    out.append('];')
    return '\n'.join(out) + '\n'


# The interface's table codes with no hand-written date format in i18n.rs, and their CLDR locale.
UI_DATES = {
    'pl': 'pl', 'pt': 'pt', 'nl': 'nl', 'tr': 'tr', 'uk': 'uk', 'cs': 'cs', 'sk': 'sk', 'sl': 'sl',
    'hr': 'hr', 'sr': 'sr', 'ro': 'ro', 'bg': 'bg', 'el': 'el', 'lt': 'lt', 'lv': 'lv', 'et': 'et',
    'fi': 'fi', 'sv': 'sv', 'nb': 'nb', 'da': 'da', 'ca': 'ca', 'gl': 'gl', 'sq': 'sq', 'az': 'az',
    'hy': 'hy', 'ka': 'ka', 'ar': 'ar', 'he': 'he', 'fa': 'fa', 'hi': 'hi', 'te': 'te', 'th': 'th',
    'vi': 'vi', 'id': 'id', 'ms': 'ms', 'ja': 'ja', 'ko': 'ko', 'zh': 'zh_Hans_CN', 'zh-TW': 'zh_Hant_TW',
}


def no_year(p):
    """CLDR's full date without the year (and era): the weekday, day and month."""
    p = re.sub(r'\s*G+\s*', ' ', p)
    p = re.sub(r"\s*'?(de|del)'?\s*y+", '', p)  # "d 'de' MMMM 'de' y"
    p = re.sub(r'y+\s*(年|년|թ\.)\s*', '', p)
    p = re.sub(r"\s*y+\.?\s*('(gada|m|г|р)'\.?)?", ' ', p)  # with "y. 'gada'", "y 'г'."
    p = re.sub(r' +([,،])', r'\1', re.sub(' +', ' ', p))
    return p.strip(' ,،')


def pattern(p):
    p = re.sub(r'\s*G+\s*', ' ', p).strip()
    return p


def gen_dates():
    out = ['//! Generated by `tools/gen-lang-data.py`: do not edit by hand.',
           '//! CLDR date formats for the interface languages without a hand-written one.',
           '', '/// (code, months, weekdays from Sunday, full date pattern, weekday + day + month pattern)',
           'pub(crate) static DATES: &[(&str, [&str; 12], [&str; 7], &str, &str)] = &[']
    for code, cl in UI_DATES.items():
        loc = Locale.parse(cl)
        months = [loc.months['format']['wide'][i] for i in range(1, 13)]
        # Babel numbers weekdays from Monday = 0.
        days = [loc.days['format']['wide'][(i + 6) % 7] for i in range(7)]
        full = pattern(loc.date_formats['long'].pattern)
        wdm = no_year(loc.date_formats['full'].pattern)
        out.append(f'    ({rs(code)}, [{", ".join(rs(m) for m in months)}], [{", ".join(rs(d) for d in days)}], {rs(full)}, {rs(wdm)}),')
    out.append('];')
    return '\n'.join(out) + '\n'


if __name__ == '__main__':
    for path, text in [('crates/yt-core/src/lang_data_gen.rs', gen_core()), ('crates/app/src/i18n_dates.rs', gen_dates())]:
        with open(os.path.join(ROOT, path), 'w', encoding='utf-8', newline='\n') as f:
            f.write(text)
        print('wrote', path)
