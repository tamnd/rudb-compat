-- fires
-- The MIN is on a leaf the filter is not on. Only the sweep back down from the root removes keyword 2, whose only movies have no company in Sweden.
SELECT MIN(k.keyword) AS keyword FROM keyword AS k, movie_keyword AS mk, title AS t, movie_companies AS mc, company_name AS cn
WHERE k.id = mk.keyword_id AND mk.movie_id = t.id AND t.id = mc.movie_id AND mc.company_id = cn.id AND cn.country_code = '[se]';
